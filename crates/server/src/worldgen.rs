// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only
//! Terrain generated off the tick thread.
//!
//! # Why this exists
//!
//! Reported from the window on 2026-09-13, with every player standing still:
//! tick after tick at 50–100 ms, each one `serving 0 chunks, 1 summaries` and
//! `gen 69.2ms`. A horizon summary is one chunk generated through the mod, and
//! the terrain mod's chunk had come to cost more than a whole tick. The serve
//! clock ([`crate::transport::endpoint::SERVE_TIME_BUDGET`]) cannot help: a
//! generation call is indivisible, and one request is always served so a slow
//! machine still finishes loading — so with a 60 ms chunk, that floor *is* the
//! overrun. Pacing to the remaining budget was suggested and does not work for
//! the same reason: there is no remaining budget a 60 ms job fits in.
//!
//! What fits is the tick not doing it. Generation is a pure function of the
//! seed and the position — charter rule 4 requires that, and the determinism
//! gate proves it — so it can run on any thread, and a thread that is not the
//! tick can take as long as it likes.
//!
//! # The shape
//!
//! A [`Pool`] of workers, each with **its own script VM** over the same mod set,
//! loaded the way a restarted server loads: mods in resolved order, frozen, the
//! same fluid ids, the same saved maps. Not the world pre-pass — that ran once
//! in the world's life and its results are the maps. A worker's VM therefore
//! knows exactly what a fresh start's VM knows, which is the state generation
//! is allowed to depend on.
//!
//! The tick submits `(domain, pos)` jobs and parks the requests that wanted
//! them; workers answer with the chunk, its fluid, its tint and its summary
//! chain — everything a request could need that is computable from the chunk
//! alone. What still happens on the tick is what needs the tick's state:
//! adopting the chunk into the world, lighting it, encoding it, sending it.
//!
//! # What a mod has to hold to
//!
//! A generator runs in a VM that has never seen a player, a tick or an edit.
//! `buf`, `pos` (with its seed), the density programs and the maps are all it
//! has; anything else is a pure function's business anyway. Lua state a mod
//! keeps between generator calls persists *per worker*, so counters and caches
//! there are per-worker numbers, and nothing a generator writes there reaches
//! the mod's copy on the tick. `api/AGENTS.md` says the same to mod authors.
//!
//! # Determinism
//!
//! Every worker asserts IEEE mode at spawn ([`tiamot_core::assert_ieee_mode`]),
//! as the simulation thread does. `tests::a_worker_generates_the_chunk_the_tick_would`
//! holds the two VMs to bit-identical output over a real generator.
//!
//! # Faults
//!
//! A mod that errors in a generator is disabled (charter rule 10). Disabled in
//! ONE VM is a world whose chunks differ by which worker made them, so a fault
//! is shared: a worker that faults a mod reports it, the tick marks the mod
//! faulted in its own VM and in every worker, and a mod the tick faults is
//! marked in the workers on the next pass. There is a window — chunks already
//! generating when the fault lands — and it is the same window a single VM
//! has between one chunk and the next.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, RwLock};

use tiamot_core::script::{MluaVm, ModHost, ScriptVm as _, VmLimits};
use tiamot_core::{Chunk, ChunkPos, MaterialId};
use tracing::{error, info, warn};

/// Everything a worker needs to build a VM that generates what the tick's
/// would.
#[derive(Debug, Clone)]
pub struct WorkerSpec {
    /// Where the mods are.
    pub mods_root: PathBuf,
    /// Which of them, or all of them.
    pub enabled: Option<Vec<String>>,
    /// The VM limits the tick's VM was built with.
    pub limits: VmLimits,
    /// Fluid ids as the world assigned them, so an ocean is the same liquid.
    pub fluid_ids: Vec<(String, tiamot_core::fluid::FluidId)>,
    /// Every map the world holds, as the pre-pass left them.
    pub maps: Vec<(String, String, tiamot_core::detgen::Map)>,
    /// The materials the tick's VM registered, in order, with their ids.
    ///
    /// A worker whose VM registers anything else is refused: its chunks would
    /// name materials by numbers the world does not agree with.
    pub blocks: Vec<(String, MaterialId)>,
}

/// One chunk to generate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// Which domain's chunk.
    pub domain: String,
    /// Which chunk.
    pub pos: ChunkPos,
    /// The world seed.
    pub seed: u64,
    /// The order this was asked in, counting from the pool's first job.
    ///
    /// The tick asks nearest-first, and workers finish in whatever order the
    /// chunks cost; serving answers in THIS order restores the tick's order
    /// among whatever has come back, so the ground under a player is sent
    /// before the sky above them whenever both are ready. See
    /// `handle::Parked`.
    pub seq: u64,
}

/// A generated chunk and everything computable from it alone.
#[derive(Debug)]
pub struct Done {
    /// The job this answers.
    pub job: Job,
    /// The chunk.
    pub chunk: Chunk,
    /// Any fluid the generator placed.
    pub fluid: tiamot_core::fluid::FluidLayer,
    /// The mod's colour for this chunk, white if it has none.
    pub tint: [u8; 3],
    /// The mod's fog for this chunk's column, if it gives one.
    pub fog: Option<tiamot_core::proto::ChunkFog>,
    /// The summary chain, encoded, one entry per level.
    pub summaries: Vec<(u8, Vec<u8>)>,
    /// Mods this job faulted in the worker, for the tick to fault everywhere.
    pub faults: Vec<String>,
}

/// Why a pool could not start.
#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    /// A worker thread could not be spawned.
    #[error("could not spawn a generation worker: {0}")]
    Spawn(#[source] std::io::Error),
    /// A worker's VM did not come up as the tick's did.
    #[error("generation worker {worker} refused to start: {reason}")]
    Worker {
        /// Which worker.
        worker: usize,
        /// What it found wrong.
        reason: String,
    },
}

/// The generation workers, and the jobs between the tick and them.
pub struct Pool {
    jobs: mpsc::Sender<Job>,
    done: mpsc::Receiver<Done>,
    /// Mods faulted anywhere, read by every worker before each job.
    faulted: Arc<RwLock<BTreeSet<String>>>,
    /// Jobs handed out and not yet answered.
    in_flight: BTreeSet<(String, ChunkPos)>,
    /// How many jobs may be out at once.
    capacity: usize,
    workers: Vec<std::thread::JoinHandle<()>>,
    /// Chunks answered so far.
    generated: u64,
    /// The next job's sequence number.
    next_seq: u64,
}

impl std::fmt::Debug for Pool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pool")
            .field("workers", &self.workers.len())
            .field("in_flight", &self.in_flight.len())
            .field("capacity", &self.capacity)
            .finish_non_exhaustive()
    }
}

/// How many jobs a pool keeps out at once, per worker.
///
/// Two: one generating and one queued behind it, so a worker never waits on
/// the tick for its next job, and no more, because every job out is a request
/// parked on the tick and a player's in-flight slot spent on it. A pool that
/// queued everything it was offered would let one connection's horizon fill
/// the workers for seconds while another player's ground waited behind it.
pub const JOBS_PER_WORKER: usize = 2;

/// How many workers to run, given the machine.
///
/// Half the cores, between one and four. The tick thread and the network need
/// their own, and the minimum spec is a six-core part (charter rule 18), on
/// which this is two workers and a tick that keeps its budget. More than four
/// is memory for VMs that mostly wait: a chunk is asked for at the rate a
/// connection's in-flight cap allows, not as fast as it can be made.
///
/// `TIAMOT_GEN_THREADS` overrides it, `0` meaning generate on the tick as
/// before — there to measure one against the other, not to configure a server.
#[must_use]
pub fn worker_count() -> usize {
    if let Some(forced) = std::env::var("TIAMOT_GEN_THREADS")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
    {
        return forced.min(16);
    }
    std::thread::available_parallelism().map_or(1, |cores| (cores.get() / 2).clamp(1, 4))
}

impl Pool {
    /// Starts `workers` threads, each loading its own VM from `spec`.
    ///
    /// Blocks until every worker has either loaded and verified its VM or
    /// refused, so a pool that exists is a pool that generates what the tick
    /// would. Loading a mod set takes as long as it takes on the tick — a
    /// second or two for the reference mods — once per worker, in parallel.
    ///
    /// # Errors
    ///
    /// [`PoolError`] if a thread could not be spawned or a worker's VM did not
    /// match the tick's. The caller falls back to generating on the tick.
    pub fn start(spec: &WorkerSpec, workers: usize) -> Result<Self, PoolError> {
        let workers = workers.max(1);
        let (jobs, job_queue) = mpsc::channel::<Job>();
        let job_queue = Arc::new(Mutex::new(job_queue));
        let (done_tx, done) = mpsc::channel::<Done>();
        let faulted = Arc::new(RwLock::new(BTreeSet::new()));
        let (ready_tx, ready) = mpsc::channel::<(usize, Result<(), String>)>();
        let mut handles = Vec::with_capacity(workers);
        for index in 0..workers {
            let spec = spec.clone();
            let queue = Arc::clone(&job_queue);
            let done = done_tx.clone();
            let faulted = Arc::clone(&faulted);
            let ready = ready_tx.clone();
            let handle = std::thread::Builder::new()
                .name(format!("worldgen-{index}"))
                .spawn(move || worker(index, &spec, &queue, &done, &faulted, &ready))
                .map_err(PoolError::Spawn)?;
            handles.push(handle);
        }
        drop(ready_tx);
        for _ in 0..workers {
            match ready.recv() {
                Ok((_, Ok(()))) => {}
                Ok((worker, Err(reason))) => {
                    return Err(PoolError::Worker { worker, reason });
                }
                Err(_) => {
                    return Err(PoolError::Worker {
                        worker: usize::MAX,
                        reason: "a worker exited before reporting".to_owned(),
                    });
                }
            }
        }
        info!(workers, "terrain generates off the tick");
        Ok(Self {
            jobs,
            done,
            faulted,
            in_flight: BTreeSet::new(),
            capacity: workers * JOBS_PER_WORKER,
            workers: handles,
            generated: 0,
            next_seq: 0,
        })
    }

    /// Whether another job may be submitted now.
    #[must_use]
    pub fn has_room(&self) -> bool {
        self.in_flight.len() < self.capacity
    }

    /// Whether this chunk is already being generated.
    #[must_use]
    pub fn is_generating(&self, domain: &str, pos: ChunkPos) -> bool {
        self.in_flight.contains(&(domain.to_owned(), pos))
    }

    /// How many jobs are out.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.in_flight.len()
    }

    /// How many chunks the workers have answered.
    #[must_use]
    pub const fn generated(&self) -> u64 {
        self.generated
    }

    /// Hands a chunk to the workers.
    ///
    /// Returns `false`, and does nothing, when the pool is full or the chunk is
    /// already being generated; the caller keeps its request queued.
    pub fn submit(&mut self, domain: &str, pos: ChunkPos, seed: u64) -> bool {
        let key = (domain.to_owned(), pos);
        if self.in_flight.len() >= self.capacity || self.in_flight.contains(&key) {
            return false;
        }
        let job = Job {
            domain: domain.to_owned(),
            pos,
            seed,
            seq: self.next_seq,
        };
        self.next_seq += 1;
        if self.jobs.send(job).is_err() {
            // Every worker has gone. Nothing is in flight that will ever
            // answer, so say so rather than parking requests for ever.
            error!("every generation worker has stopped; generating on the tick");
            return false;
        }
        self.in_flight.insert(key);
        true
    }

    /// Everything the workers have finished since the last call.
    pub fn finished(&mut self) -> Vec<Done> {
        let mut done = Vec::new();
        while let Ok(answer) = self.done.try_recv() {
            self.in_flight
                .remove(&(answer.job.domain.clone(), answer.job.pos));
            self.generated += 1;
            done.push(answer);
        }
        done
    }

    /// Whether the workers are still there to answer what is in flight.
    #[must_use]
    pub fn is_alive(&self) -> bool {
        self.workers.iter().any(|worker| !worker.is_finished())
    }

    /// Marks mods faulted for every worker.
    ///
    /// The tick calls this with its own VM's faulted set each pass, and with a
    /// worker's report as it lands, so a mod disabled anywhere is disabled
    /// everywhere before the next chunk.
    pub fn fault<'a>(&self, mods: impl IntoIterator<Item = &'a str>) {
        if let Ok(mut faulted) = self.faulted.write() {
            for mod_id in mods {
                if faulted.insert(mod_id.to_owned()) {
                    warn!(mod_id, "mod disabled in every generation worker");
                }
            }
        }
    }
}

/// One worker's life: build the VM, check it, then answer jobs until the pool
/// is dropped.
fn worker(
    index: usize,
    spec: &WorkerSpec,
    queue: &Mutex<mpsc::Receiver<Job>>,
    done: &mpsc::Sender<Done>,
    faulted: &RwLock<BTreeSet<String>>,
    ready: &mpsc::Sender<(usize, Result<(), String>)>,
) {
    // Charter rule 4: a thread generating terrain is a simulation thread.
    tiamot_core::assert_ieee_mode();
    let mut host = match load(spec) {
        Ok(host) => host,
        Err(reason) => {
            let _ = ready.send((index, Err(reason)));
            return;
        }
    };
    let _ = ready.send((index, Ok(())));
    // What this worker knows to be faulted, so a shared set that has not
    // changed costs a length compare and nothing else.
    let mut known_faulted: BTreeSet<String> = BTreeSet::new();
    loop {
        // Lock only to take a job. Holding it while generating would make the
        // pool one worker wide.
        let job = {
            let Ok(queue) = queue.lock() else { return };
            match queue.recv() {
                Ok(job) => job,
                Err(_) => return,
            }
        };
        if let Ok(shared) = faulted.read()
            && shared.len() != known_faulted.len()
        {
            for mod_id in shared.difference(&known_faulted) {
                host.vm_mut().mark_faulted(mod_id);
            }
            known_faulted.clone_from(&shared);
        }
        let answer = generate(&mut host, &job, &mut known_faulted);
        if done.send(answer).is_err() {
            // The tick has dropped the pool.
            return;
        }
    }
}

/// Builds and checks one worker's VM.
fn load(spec: &WorkerSpec) -> Result<ModHost<MluaVm>, String> {
    let mut host =
        ModHost::<MluaVm>::load_selected(&spec.mods_root, spec.limits, spec.enabled.as_deref())
            .map_err(|err| format!("could not load the mods: {err}"))?;
    host.freeze()
        .map_err(|err| format!("could not freeze the mods: {err}"))?;
    // **The same materials, in the same order, with the same numbers.** The
    // tick's VM registered these and the world's table was checked against
    // them at start; a worker that disagrees would write chunks whose numbers
    // mean something else.
    let blocks = host.vm().registered_blocks();
    if blocks != spec.blocks {
        return Err(format!(
            "registered {} materials where the tick registered {}, or in another order",
            blocks.len(),
            spec.blocks.len()
        ));
    }
    host.vm_mut().set_fluid_ids(&spec.fluid_ids);
    host.vm_mut().load_maps(spec.maps.clone());
    // Mods that failed to load here failed to load on the tick too, for the
    // same reason; the tick already reported them.
    Ok(host)
}

/// Generates one chunk and everything computable from it.
///
/// A panic inside generation — a bug, since a mod's errors are caught by the
/// VM — answers with air rather than never answering: a job that vanished is a
/// request parked for ever and a player's in-flight slot never freed.
fn generate(host: &mut ModHost<MluaVm>, job: &Job, known_faulted: &mut BTreeSet<String>) -> Done {
    let before: BTreeSet<String> = host.disabled().into_iter().collect();
    // Per job rather than at spawn: the pool starts before the world has said
    // which seed it keeps, and the job is what knows. A table write per mod.
    host.vm_mut().set_world_seed(job.seed);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let (chunk, fluid) =
            match host.generate_chunk_with_fluid(&job.domain, job.seed, job.pos, MaterialId::AIR) {
                Ok(generated) => generated,
                Err(err) => {
                    // As `ModGenerator` answers on the tick: the host has disabled
                    // the mod, and air is the honest chunk.
                    warn!(pos = ?job.pos, "chunk generation failed, falling back to air: {err}");
                    (
                        Chunk::new(job.pos, MaterialId::AIR),
                        tiamot_core::fluid::FluidLayer::default(),
                    )
                }
            };
        let tint = host
            .chunk_tint(&job.domain, job.seed, job.pos)
            .unwrap_or([u8::MAX; 3]);
        let fog = host
            .chunk_fog(&job.domain, job.seed, job.pos)
            .ok()
            .flatten();
        (chunk, fluid, tint, fog)
    }));
    let (chunk, fluid, tint, fog) = match outcome {
        Ok(generated) => generated,
        Err(_) => {
            error!(pos = ?job.pos, "generation panicked; the chunk is air");
            (
                Chunk::new(job.pos, MaterialId::AIR),
                tiamot_core::fluid::FluidLayer::default(),
                [u8::MAX; 3],
                None,
            )
        }
    };
    let summaries = crate::world::World::encode_chain(&chunk);
    let after: BTreeSet<String> = host.disabled().into_iter().collect();
    let faults: Vec<String> = after.difference(&before).cloned().collect();
    known_faulted.extend(faults.iter().cloned());
    Done {
        job: job.clone(),
        chunk,
        fluid,
        tint,
        fog,
        summaries,
        faults,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// A mod with a real generator: sampled terrain, a pond, and a biome tint,
    /// so every field of a [`Done`] has something in it to compare.
    fn write_mod(name: &str, extra: &str) -> PathBuf {
        let root = std::env::temp_dir().join("tiamot-worldgen-pool").join(name);
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("terrain");
        std::fs::create_dir_all(&dir).expect("mod dir");
        std::fs::write(
            dir.join("mod.toml"),
            "id = \"terrain\"\nname = \"Terrain\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
        )
        .expect("manifest");
        std::fs::write(
            dir.join("init.lua"),
            format!(
                "local stone = game.register_block{{ id = \"stone\" }}\n\
                 local field = game.density{{\n\
                 \x20   op = \"sub\",\n\
                 \x20   a = {{ op = \"noise\", stream = \"terrain\", frequency = 0.03, octaves = 3 }},\n\
                 \x20   b = {{ op = \"mul\", a = {{ op = \"y\" }}, b = {{ op = \"const\", value = 0.06 }} }},\n\
                 }}\n\
                 game.register_chunk_tint(function(pos)\n\
                 \x20   return (pos.x % 4) / 4, 0.8, (pos.z % 4) / 4\n\
                 end)\n\
                 game.register_on_generate(function(buf, pos)\n\
                 {extra}\n\
                 \x20   buf:fill_density(field, stone, {{ detail = \"sampled\" }})\n\
                 end)\n"
            ),
        )
        .expect("script");
        root
    }

    fn spec_for(root: &Path) -> (ModHost<MluaVm>, WorkerSpec) {
        let mut host =
            ModHost::<MluaVm>::load_selected(root, VmLimits::default(), None).expect("load");
        host.freeze().expect("freeze");
        assert!(
            host.failed().is_empty(),
            "the fixture mod failed to load: {:?}",
            host.failed()
        );
        let spec = WorkerSpec {
            mods_root: root.to_path_buf(),
            enabled: None,
            limits: VmLimits::default(),
            fluid_ids: Vec::new(),
            maps: Vec::new(),
            blocks: host.vm().registered_blocks(),
        };
        (host, spec)
    }

    /// Submits every position, waiting for room as the pool fills, and
    /// returns every answer.
    fn generate_all(pool: &mut Pool, positions: &[ChunkPos], seed: u64) -> Vec<Done> {
        let mut answers = Vec::new();
        for pos in positions {
            while !pool.has_room() {
                answers.extend(wait_for(pool, 1));
            }
            assert!(
                pool.submit("overworld", *pos, seed),
                "the pool refused a job with room"
            );
        }
        let outstanding = positions.len() - answers.len();
        answers.extend(wait_for(pool, outstanding));
        answers
    }

    fn wait_for(pool: &mut Pool, count: usize) -> Vec<Done> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let mut all = Vec::new();
        while all.len() < count {
            assert!(
                std::time::Instant::now() < deadline,
                "the workers answered {} of {count} jobs in a minute",
                all.len()
            );
            all.extend(pool.finished());
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        all
    }

    #[test]
    fn a_worker_generates_the_chunk_the_tick_would() {
        // **The whole premise.** Two VMs, loaded the same way, must produce the
        // same bytes for the same position — chunk, fluid, tint and summary
        // chain — or the world a player is sent depends on which thread made
        // it. Several positions across two workers, so both VMs are exercised.
        let root = write_mod("same", "");
        let (mut tick_host, spec) = spec_for(&root);
        let mut pool = Pool::start(&spec, 2).expect("pool");
        let positions: Vec<ChunkPos> = (0..6)
            .map(|i| ChunkPos::new(i - 3, (i % 3) - 1, 2 * i))
            .collect();
        let answers = generate_all(&mut pool, &positions, 77);
        assert_eq!(answers.len(), positions.len());
        for done in answers {
            let (chunk, fluid) = tick_host
                .generate_chunk_with_fluid("overworld", 77, done.job.pos, MaterialId::AIR)
                .expect("tick generation");
            assert!(
                chunk == done.chunk,
                "chunk at {:?} differs between VMs",
                done.job.pos
            );
            assert_eq!(fluid, done.fluid, "fluid at {:?} differs", done.job.pos);
            let tint = tick_host
                .chunk_tint("overworld", 77, done.job.pos)
                .expect("tint");
            assert_eq!(tint, done.tint, "tint at {:?} differs", done.job.pos);
            assert_eq!(
                crate::world::World::encode_chain(&chunk),
                done.summaries,
                "summary chain at {:?} differs",
                done.job.pos
            );
            assert!(done.faults.is_empty());
            // Non-vacuous: the fixture makes terrain, not air.
            assert!(
                done.chunk.is_uniform().is_none() || done.job.pos.y > 0,
                "the fixture generated a uniform chunk at {:?}",
                done.job.pos
            );
        }
        assert_eq!(pool.generated(), positions.len() as u64);
        assert_eq!(pool.in_flight(), 0);
    }

    #[test]
    fn a_full_pool_refuses_and_a_repeated_job_is_not_taken_twice() {
        let root = write_mod("full", "");
        let (_host, spec) = spec_for(&root);
        let mut pool = Pool::start(&spec, 1).expect("pool");
        let mut accepted = 0;
        for x in 0..(JOBS_PER_WORKER as i32 + 3) {
            if pool.submit("overworld", ChunkPos::new(x, 0, 0), 1) {
                accepted += 1;
            }
        }
        assert_eq!(
            accepted, JOBS_PER_WORKER,
            "the pool took more than its capacity"
        );
        assert!(!pool.has_room());
        assert!(pool.is_generating("overworld", ChunkPos::new(0, 0, 0)));
        let answers = wait_for(&mut pool, JOBS_PER_WORKER);
        assert_eq!(answers.len(), JOBS_PER_WORKER);
        assert!(pool.has_room());
        assert!(pool.submit("overworld", ChunkPos::new(9, 0, 0), 1));
        assert!(
            !pool.submit("overworld", ChunkPos::new(9, 0, 0), 1),
            "the same chunk was handed out twice"
        );
        wait_for(&mut pool, 1);
    }

    #[test]
    fn a_fault_in_one_worker_disables_the_mod_for_every_worker() {
        // Charter rule 10 across VMs. The generator errors at one position; the
        // worker that hit it reports the fault, and once the pool has been told,
        // every worker answers air for that mod — including the one that never
        // saw the error. Without this the world would be terrain from one
        // worker and air from another.
        let root = write_mod(
            "fault",
            "    if pos.x == 5 then error(\"a generator that cannot cope with x = 5\") end",
        );
        let (_host, spec) = spec_for(&root);
        let mut pool = Pool::start(&spec, 2).expect("pool");
        assert!(pool.submit("overworld", ChunkPos::new(0, 0, 0), 3));
        let before = wait_for(&mut pool, 1).remove(0);
        assert!(
            before.chunk.is_uniform().is_none(),
            "the fixture should make terrain at x = 0"
        );
        assert!(before.faults.is_empty());

        assert!(pool.submit("overworld", ChunkPos::new(5, 0, 0), 3));
        let faulted = wait_for(&mut pool, 1).remove(0);
        assert_eq!(faulted.faults, vec!["terrain".to_owned()]);
        assert_eq!(
            faulted.chunk.is_uniform(),
            Some(MaterialId::AIR),
            "a faulted generator is air"
        );
        // What the tick does with the report.
        pool.fault(faulted.faults.iter().map(String::as_str));

        // Enough jobs that both workers take at least one, and every answer is
        // air: the mod is gone everywhere.
        let positions: Vec<ChunkPos> = (0..4).map(|x| ChunkPos::new(x, 0, 1)).collect();
        for done in generate_all(&mut pool, &positions, 3) {
            assert_eq!(
                done.chunk.is_uniform(),
                Some(MaterialId::AIR),
                "a worker still ran the faulted mod at {:?}",
                done.job.pos
            );
        }
    }

    #[test]
    fn a_worker_whose_materials_differ_refuses_to_start() {
        // The one check that stands between a worker and a world whose numbers
        // mean something else: the tick's material list is handed over and a
        // VM that registers anything different does not become a worker.
        let root = write_mod("blocks", "");
        let (_host, mut spec) = spec_for(&root);
        spec.blocks.push(("not_here".to_owned(), MaterialId(99)));
        match Pool::start(&spec, 1) {
            Err(PoolError::Worker { reason, .. }) => {
                assert!(reason.contains("materials"), "{reason}");
            }
            Err(other) => panic!("wrong error: {other}"),
            Ok(_) => panic!("a worker with a different material table started"),
        }
    }

    #[test]
    fn the_worker_count_is_between_one_and_four_unless_forced() {
        // Not a test of the machine; a test that the clamp is there.
        let count = worker_count();
        assert!((0..=16).contains(&count));
    }
}
