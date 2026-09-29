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
//! Every worker asserts IEEE mode at spawn ([`tiamat_core::assert_ieee_mode`]),
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
use std::time::{Duration, Instant};

use tiamat_core::script::{MluaVm, ModHost, ScriptVm as _, VmLimits};
use tiamat_core::{Chunk, ChunkPos, MaterialId};
use tracing::{error, info, warn};

/// Everything a worker needs to build a VM that generates what the tick's
/// would.
#[derive(Debug, Clone)]
pub struct WorkerSpec {
    /// Where the mods are.
    pub mods_root: PathBuf,
    /// Which of them, or all of them.
    pub enabled: Option<Vec<String>>,
    /// What the world chose for its mods' world options, exactly as the tick
    /// loaded its own VM with, so both resolve to the same list — a generator
    /// that read one answer here and another there would be two worlds.
    pub world_options: Vec<(String, String)>,
    /// The VM limits the tick's VM was built with.
    pub limits: VmLimits,
    /// Fluid ids as the world assigned them, so an ocean is the same liquid.
    pub fluid_ids: Vec<(String, tiamat_core::fluid::FluidId)>,
    /// The fluids as the world registered them, for the summary chain.
    ///
    /// **A worker encodes its chunk's summaries**, and a summary carries the
    /// sea (World ask 28) — which means turning a fluid id into the block it is
    /// drawn as. A worker's own VM could be asked, but the answer has to be the
    /// world's rather than the worker's, for exactly the reason `blocks` is
    /// here: a worker that numbered anything differently would encode a horizon
    /// the world does not agree with.
    pub fluids: tiamat_core::fluid::Fluids,
    /// Every map the world holds, as the pre-pass left them.
    pub maps: Vec<(String, String, tiamat_core::detgen::Map)>,
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
    pub fluid: tiamat_core::fluid::FluidLayer,
    /// The mod's colour for this chunk, white if it has none.
    pub tint: [u8; 3],
    /// The mod's fog for this chunk's column, if it gives one.
    pub fog: Option<tiamat_core::proto::ChunkFog>,
    /// The summary chain, encoded, one entry per level.
    pub summaries: Vec<(u8, Vec<u8>)>,
    /// Mods this job faulted in the worker, for the tick to fault everywhere.
    pub faults: Vec<String>,
    /// How long the worker spent generating it.
    ///
    /// World ask 40: the designer flew a world arriving as slabs and the
    /// log said nothing, because the over-budget warning watches the tick
    /// and the workers exist to keep generation off it. Timed here so the
    /// pool can say what a chunk costs.
    pub took: std::time::Duration,
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
    /// How long the last few chunks took, in microseconds, newest last.
    ///
    /// A rolling window rather than a lifetime average: what a chunk costs
    /// changes with the country a player is over, and the line that reports
    /// it (World ask 40) is about now.
    recent_micros: std::collections::VecDeque<u64>,
    /// What each worker has spent generating, the job in hand included.
    clocks: Arc<[Mutex<Busy>]>,
}

/// What one worker has spent generating, for [`Pool::busy`].
///
/// **The job in hand counts up to now**, not only the ones finished. A chunk
/// that costs longer than a look would otherwise leave a look with nothing
/// finished in it — a saturated pool reading as an idle one, exactly when
/// generation is furthest behind.
#[derive(Debug, Default)]
struct Busy {
    /// Time on the jobs it has finished.
    spent: Duration,
    /// When the job in hand began, if it has one.
    since: Option<Instant>,
}

/// How many recent chunks the pool averages its cost over.
const RECENT_CHUNKS: usize = 64;

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
/// `TIAMAT_GEN_THREADS` overrides it, `0` meaning generate on the tick as
/// before — there to measure one against the other, not to configure a server.
#[must_use]
pub fn worker_count() -> usize {
    if let Some(forced) = std::env::var("TIAMAT_GEN_THREADS")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
    {
        return forced.min(16);
    }
    std::thread::available_parallelism().map_or(1, |cores| (cores.get() / 2).clamp(1, 4))
}

/// Looks in a row the workers must have been saturated before the lag line
/// speaks: three seconds, at the tick's one look a second.
pub const LAG_LOOKS: u32 = 3;

/// The share of the workers' time, in percent, spent generating at or above
/// which the pool counts as saturated.
///
/// Not a hundred: a worker finishing its second job a moment before the tick
/// hands it a third is idle for that moment, and a pool doing nothing but
/// generating shows a few percent of such gaps. Well above what a pool the
/// tick is pacing shows, because that pool idles for most of every tick — see
/// [`Lag`].
pub const LAG_BUSY_PERCENT: u64 = 90;

/// Whether generation is keeping up with the players asking for it.
///
/// # What "behind" means
///
/// **Generation is behind when it sets the pace the world arrives at: the
/// workers spent (nearly) all their time generating, and requests still
/// waited.** For [`LAG_LOOKS`] looks running, so a burst — a join, a teleport —
/// that the pool drains within a couple of seconds says nothing.
///
/// Counting requests cannot tell that apart from a stream being answered, which
/// is how the two rules before this one failed. World ask 40 shipped comparing
/// the backlog with the pool's capacity, four workers of [`JOBS_PER_WORKER`],
/// eight — but one player's request window is larger than that, so a single
/// player walking always had more waiting than the pool held, and the line
/// fired at `warn` once a second on every walk, at 0.6 ms a chunk as readily as
/// at 45. The next rule waited for the backlog to fill every streaming
/// player's whole window, which a stalled stream cannot do: a connection's
/// chunks and summaries share one cap, the horizon waits behind the detail, and
/// an idle second player's window counted towards the bound while adding
/// nothing to the backlog. It never fired, however slow the generator.
///
/// What does tell them apart is **where the time goes**. The tick hands the
/// pool at most its capacity per pass, so a pool of cheap chunks is full at
/// every look and idle for most of every tick — the tick's pace is the stream's
/// limit then, not the workers. Only when a worker's jobs outlast the pass that
/// refills them — a chunk dearer than half a tick, at two jobs a worker — do
/// they generate without pause, and then every millisecond more a chunk costs
/// is that much longer for the world to arrive. Nothing about who is
/// connected, or how many requests each may have out, enters it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Lag {
    /// Looks in a row the workers have been saturated with requests waiting.
    behind_for: u32,
    /// When that run began: the look before its first, since a look's share
    /// covers the time back to the one before it.
    began: Option<Instant>,
    /// When the last look was, and the workers' busy total then.
    last: Option<(Instant, Duration)>,
}

/// What the lag line has to say: generation is behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Behind {
    /// Looks in a row the workers have been saturated.
    pub looks: u32,
    /// How long that has been, by the clock rather than by counting looks: a
    /// tick running over spaces the looks out.
    pub lasted: Duration,
    /// The share of the workers' time spent generating since the last look,
    /// in percent.
    pub busy_percent: u64,
}

impl Lag {
    /// One look at the pool: the moment, the workers' busy total then
    /// ([`Pool::busy`]), how many workers there are, and how many requests are
    /// waiting on generation.
    ///
    /// `Some` when the line should speak. The first look only sets the
    /// baseline, since a share needs two readings.
    pub fn look(
        &mut self,
        at: Instant,
        busy: Duration,
        workers: usize,
        waiting: usize,
    ) -> Option<Behind> {
        let Some((then, before)) = self.last.replace((at, busy)) else {
            self.behind_for = 0;
            self.began = None;
            return None;
        };
        let available = at
            .saturating_duration_since(then)
            .as_micros()
            .saturating_mul(workers as u128);
        let spent = busy.saturating_sub(before).as_micros();
        // No time between the looks, or no workers, is no share at all. Over a
        // hundred is only the moment between reading the clock and the
        // workers, and it says "all of it".
        let percent = spent
            .saturating_mul(100)
            .checked_div(available)
            .map_or(0, |share| u64::try_from(share.min(100)).unwrap_or(100));
        if waiting > 0 && percent >= LAG_BUSY_PERCENT {
            if self.behind_for == 0 {
                self.began = Some(then);
            }
            self.behind_for = self.behind_for.saturating_add(1);
        } else {
            self.behind_for = 0;
            self.began = None;
        }
        (self.behind_for >= LAG_LOOKS).then(|| Behind {
            looks: self.behind_for,
            lasted: self
                .began
                .map_or(Duration::ZERO, |began| at.saturating_duration_since(began)),
            busy_percent: percent,
        })
    }
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
        let clocks: Arc<[Mutex<Busy>]> = (0..workers).map(|_| Mutex::default()).collect();
        let (ready_tx, ready) = mpsc::channel::<(usize, Result<(), String>)>();
        let mut handles = Vec::with_capacity(workers);
        for index in 0..workers {
            let spec = spec.clone();
            let queue = Arc::clone(&job_queue);
            let done = done_tx.clone();
            let faulted = Arc::clone(&faulted);
            let clocks = Arc::clone(&clocks);
            let ready = ready_tx.clone();
            let handle = std::thread::Builder::new()
                .name(format!("worldgen-{index}"))
                .spawn(move || {
                    worker(
                        index,
                        &spec,
                        &queue,
                        &done,
                        &faulted,
                        &ready,
                        &clocks[index],
                    );
                })
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
            recent_micros: std::collections::VecDeque::with_capacity(RECENT_CHUNKS),
            clocks,
        })
    }

    /// How many workers generate.
    #[must_use]
    pub fn workers(&self) -> usize {
        self.workers.len()
    }

    /// How long the workers have spent generating, all of them together,
    /// since the pool started — each one's job in hand counted up to `now`.
    ///
    /// Two readings a known time apart give the share of the workers' time
    /// that went on generation, which is what [`Lag`] calls behind.
    #[must_use]
    pub fn busy(&self, now: Instant) -> Duration {
        self.clocks
            .iter()
            .filter_map(|clock| clock.lock().ok())
            .map(|clock| {
                clock.spent
                    + clock
                        .since
                        .map_or(Duration::ZERO, |since| now.saturating_duration_since(since))
            })
            .sum()
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

    /// How many jobs may be out at once.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// What a chunk has cost lately, averaged over the last few answered.
    ///
    /// `None` until one has been answered.
    #[must_use]
    pub fn recent_cost(&self) -> Option<std::time::Duration> {
        if self.recent_micros.is_empty() {
            return None;
        }
        let total: u64 = self.recent_micros.iter().sum();
        Some(std::time::Duration::from_micros(
            total / self.recent_micros.len() as u64,
        ))
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
            let micros = u64::try_from(answer.took.as_micros()).unwrap_or(u64::MAX);
            if self.recent_micros.len() == RECENT_CHUNKS {
                self.recent_micros.pop_front();
            }
            self.recent_micros.push_back(micros);
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
    clock: &Mutex<Busy>,
) {
    // Charter rule 4: a thread generating terrain is a simulation thread.
    tiamat_core::assert_ieee_mode();
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
        let started = Instant::now();
        if let Ok(mut clock) = clock.lock() {
            clock.since = Some(started);
        }
        let mut answer = generate(&mut host, &job, &spec.fluids, &mut known_faulted);
        answer.took = started.elapsed();
        // Under the one lock, so a reading never counts this job both as
        // finished and as in hand.
        if let Ok(mut clock) = clock.lock() {
            clock.spent += answer.took;
            clock.since = None;
        }
        if done.send(answer).is_err() {
            // The tick has dropped the pool.
            return;
        }
    }
}

/// Builds and checks one worker's VM.
fn load(spec: &WorkerSpec) -> Result<ModHost<MluaVm>, String> {
    let mut host = ModHost::<MluaVm>::load_selected_with_options(
        &spec.mods_root,
        spec.limits,
        spec.enabled.as_deref(),
        &spec.world_options,
    )
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
fn generate(
    host: &mut ModHost<MluaVm>,
    job: &Job,
    fluids: &tiamat_core::fluid::Fluids,
    known_faulted: &mut BTreeSet<String>,
) -> Done {
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
                        tiamat_core::fluid::FluidLayer::default(),
                    )
                }
            };
        let tint = host
            .chunk_tint(&job.domain, job.seed, job.pos)
            .unwrap_or(tiamat_core::proto::Tint::NEUTRAL);
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
                tiamat_core::fluid::FluidLayer::default(),
                tiamat_core::proto::Tint::NEUTRAL,
                None,
            )
        }
    };
    let summaries = crate::world::World::encode_chain(&chunk, &fluid, fluids);
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
        took: std::time::Duration::ZERO,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// Instants a second apart, as the tick looks.
    fn seconds_from_now() -> impl Iterator<Item = Instant> {
        let start = Instant::now();
        (0u64..).map(move |second| start + Duration::from_secs(second))
    }

    /// What four workers spend in a second when the tick hands them eight
    /// chunks a pass, twenty passes a second, at `cost` apiece.
    fn a_paced_second(cost: Duration) -> Duration {
        cost * u32::try_from(4 * JOBS_PER_WORKER * 20).expect("small")
    }

    #[test]
    fn a_stream_the_workers_keep_up_with_is_not_behind() {
        // The shape the first rule warned on once a second: nine to twelve
        // requests waiting on a four-worker pool that is full at every look.
        // At the fen soak's 0.6 ms a chunk the workers idle for nearly all of
        // every tick, and at 20 ms they still idle a fifth of it — the tick's
        // pace is what the stream waits on, not generation.
        for cost in [Duration::from_micros(600), Duration::from_millis(20)] {
            let mut lag = Lag::default();
            let mut busy = Duration::ZERO;
            for (look, at) in seconds_from_now().take(60).enumerate() {
                let waiting = 9 + look % 4;
                assert_eq!(
                    lag.look(at, busy, 4, waiting),
                    None,
                    "{waiting} waiting at {cost:?} a chunk was called behind"
                );
                busy += a_paced_second(cost);
            }
        }
    }

    #[test]
    fn workers_generating_without_pause_while_requests_wait_are_behind() {
        // The case the second rule could not see: one player, a generator at
        // 40 ms a chunk on a two-worker pool, and the eleven requests a stalled
        // connection can have out. Both workers generate the whole second.
        // Whether anybody else is connected, idle or not, is not an input.
        let workers = 2;
        let flat_out = Duration::from_secs(1) * 2;
        let mut lag = Lag::default();
        let mut busy = Duration::ZERO;
        let mut at = seconds_from_now();
        let mut look = |lag: &mut Lag, busy: Duration| {
            lag.look(at.next().expect("endless"), busy, workers, 11)
        };
        assert_eq!(look(&mut lag, busy), None, "the first look is a baseline");
        for _ in 1..LAG_LOOKS {
            busy += flat_out;
            assert_eq!(look(&mut lag, busy), None, "a burst is not a backlog");
        }
        busy += flat_out;
        assert_eq!(
            look(&mut lag, busy),
            Some(Behind {
                looks: LAG_LOOKS,
                lasted: Duration::from_secs(u64::from(LAG_LOOKS)),
                busy_percent: 100
            })
        );
        busy += flat_out;
        assert_eq!(
            look(&mut lag, busy).map(|behind| behind.looks),
            Some(LAG_LOOKS + 1),
            "it keeps saying so, and for how long"
        );
        // A second the workers spent half idle ends the run, and the count
        // starts again from nothing.
        busy += flat_out / 2;
        assert_eq!(look(&mut lag, busy), None);
        busy += flat_out;
        assert_eq!(look(&mut lag, busy), None);
    }

    #[test]
    fn idle_workers_are_not_behind_however_much_waits() {
        // A thousand requests behind workers generating 40% of the time are
        // waiting on the tick — its pace, the serving clock, the lighting —
        // and the over-budget line is the one that says so.
        let mut lag = Lag::default();
        let mut busy = Duration::ZERO;
        for at in seconds_from_now().take(10) {
            assert_eq!(lag.look(at, busy, 4, 1000), None);
            busy += Duration::from_millis(1600);
        }
    }

    #[test]
    fn busy_workers_with_nothing_waiting_or_no_time_passed_are_not_behind() {
        let mut lag = Lag::default();
        let mut busy = Duration::ZERO;
        for at in seconds_from_now().take(10) {
            assert_eq!(lag.look(at, busy, 1, 0), None, "nobody is waiting");
            busy += Duration::from_secs(1);
        }
        // Two looks at one instant, and a pool with no workers: no share to
        // take, and nothing divided by zero.
        let at = Instant::now();
        let mut lag = Lag::default();
        for _ in 0..10 {
            assert_eq!(lag.look(at, busy, 1, 5), None);
            assert_eq!(lag.look(at + Duration::from_secs(1), busy, 0, 5), None);
        }
    }

    /// A mod with a real generator: sampled terrain, a pond, and a biome tint,
    /// so every field of a [`Done`] has something in it to compare.
    fn write_mod(name: &str, extra: &str) -> PathBuf {
        let root = std::env::temp_dir().join("tiamat-worldgen-pool").join(name);
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
            world_options: Vec::new(),
            mods_root: root.to_path_buf(),
            enabled: None,
            limits: VmLimits::default(),
            fluid_ids: Vec::new(),
            fluids: tiamat_core::fluid::Fluids::new(),
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
    fn the_pool_says_what_a_chunk_has_been_costing() {
        // World ask 40: the line that reports a backlog needs a cost beside
        // the depth, and the workers are the only ones who can time it.
        let root = write_mod("cost", "");
        let (_host, spec) = spec_for(&root);
        let mut pool = Pool::start(&spec, 1).expect("pool");
        assert!(pool.recent_cost().is_none(), "nothing answered yet");
        assert!(pool.capacity() >= 1);
        let answers = generate_all(
            &mut pool,
            &[ChunkPos::new(0, 0, 0), ChunkPos::new(1, 0, 0)],
            7,
        );
        assert_eq!(answers.len(), 2);
        assert!(
            pool.recent_cost().is_some(),
            "two chunks answered and no cost to report"
        );
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
                crate::world::World::encode_chain(&chunk, &fluid, &spec.fluids),
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

    /// A generator that takes a while — a tenth of a second or so of empty Lua
    /// loop before the terrain, with the instruction cap lifted to allow it.
    fn slow_spec(name: &str) -> WorkerSpec {
        let root = write_mod(name, "    for _ = 1, 20000000 do end");
        let (_host, mut spec) = spec_for(&root);
        spec.limits.instructions_per_call = u32::MAX;
        spec
    }

    #[test]
    fn a_chunk_in_hand_counts_as_busy_before_it_finishes() {
        // The lag rule's input. A chunk that outlasts a look must still read
        // as time spent, or the pool furthest behind — every job longer than
        // a second — would read as one doing nothing.
        let mut pool = Pool::start(&slow_spec("in-hand"), 1).expect("pool");
        assert_eq!(pool.busy(Instant::now()), Duration::ZERO);
        assert!(pool.submit("overworld", ChunkPos::new(0, 0, 0), 1));
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut in_hand = Vec::new();
        let done = loop {
            let now = Instant::now();
            assert!(now < deadline, "one slow chunk took a minute");
            let busy = pool.busy(now);
            if let Some(done) = pool.finished().pop() {
                break done;
            }
            if busy > Duration::ZERO {
                in_hand.push(busy);
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        assert!(
            in_hand.len() >= 2,
            "the chunk took {:?} and was seen generating {} times",
            done.took,
            in_hand.len()
        );
        assert!(
            in_hand.windows(2).all(|pair| pair[0] < pair[1]),
            "busy did not rise while the chunk generated: {in_hand:?}"
        );
        // Finished, it is exactly what the worker timed, and it stops rising.
        let after = pool.busy(Instant::now());
        assert_eq!(after, done.took);
        std::thread::sleep(Duration::from_millis(10));
        assert_eq!(pool.busy(Instant::now()), after, "nothing in hand");
    }

    #[test]
    fn a_pool_that_never_pauses_is_behind() {
        // Kept full of slow chunks, as a tick behind a stalled stream keeps
        // it, looked at every 150 ms rather than every second. The eleven
        // waiting are what one connection can have out; nothing else about
        // the clients reaches the rule.
        let mut pool = Pool::start(&slow_spec("never-pauses"), 1).expect("pool");
        let mut lag = Lag::default();
        let mut next = 0;
        let mut said = None;
        for _ in 0..20 {
            let until = Instant::now() + Duration::from_millis(150);
            while Instant::now() < until {
                pool.finished();
                while pool.has_room() {
                    assert!(pool.submit("overworld", ChunkPos::new(next, 0, 0), 1));
                    next += 1;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            let now = Instant::now();
            said = lag.look(now, pool.busy(now), pool.workers(), 11);
            if said.is_some() {
                break;
            }
        }
        let behind = said.expect("a pool generating without pause was never called behind");
        assert_eq!(behind.looks, LAG_LOOKS);
        assert!(behind.busy_percent >= LAG_BUSY_PERCENT, "{behind:?}");
    }

    #[test]
    fn a_pool_the_tick_paces_is_not_behind() {
        // The first rule's false alarm, on a real pool: refilled once a tick,
        // full at every look, eleven requests waiting — and cheap chunks, so
        // the workers are idle for nearly all of every tick.
        let root = write_mod("paced", "    do return end");
        let (_host, spec) = spec_for(&root);
        let mut pool = Pool::start(&spec, 1).expect("pool");
        let mut lag = Lag::default();
        let mut next = 0;
        for look in 0..6 {
            for _ in 0..4 {
                pool.finished();
                while pool.has_room() {
                    assert!(pool.submit("overworld", ChunkPos::new(next, 0, 0), 1));
                    next += 1;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            pool.finished();
            while pool.has_room() {
                assert!(pool.submit("overworld", ChunkPos::new(next, 0, 0), 1));
                next += 1;
            }
            assert!(
                !pool.has_room(),
                "the premise: the pool is full at the look"
            );
            let now = Instant::now();
            assert_eq!(
                lag.look(now, pool.busy(now), pool.workers(), 11),
                None,
                "look {look}: a paced pool of cheap chunks was called behind"
            );
        }
    }
}
