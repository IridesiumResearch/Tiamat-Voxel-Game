// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only
//! Meshing off the frame thread (milestone 0.5.0, P2).
//!
//! # Why
//!
//! A lit terrain chunk meshes in 0.75 ms and a chiselled one in 2.9 ms
//! (`cargo bench -p client --bench mesher`), against a remesh budget of 2 ms
//! a frame: two chunks a frame on the reference machine, one on a
//! minimum-spec one, where the server can serve twenty-two a tick. While
//! flying, the frame thread was the streaming bottleneck, and every frame
//! that meshed paid for it in pacing. The mesher is pure — a job reads a
//! snapshot of one chunk, its neighbours, their light and their fluid, and
//! writes vertices — so the work moves to a pool of threads and the frame
//! thread keeps only what must be on it: taking the snapshot, which is a
//! few dozen pointer clones, and the upload, which is a tenth of a
//! millisecond.
//!
//! # What stays on the frame thread
//!
//! **An edit's chunks.** A chunk drawn without the neighbour whose face its
//! edit exposed is a hole through the world, so the chunks one frame's edits
//! touched are meshed on the frame, synchronously, as they always were. A
//! worker would show the hole for the frame or two the job took. Everything
//! else is streaming, whose old mesh is still right while the new one is on
//! its way.
//!
//! # Staleness
//!
//! The store counts a generation per chunk, bumped whenever the chunk is
//! marked for a remesh — its own content, a neighbour's, its light, its
//! fluid. A job carries the generation it was taken at, and a result whose
//! generation is no longer the chunk's is dropped: the chunk is dirty again
//! and a fresh job follows. A result for a chunk that has since left the
//! store is dropped too.
use std::sync::Arc;
use std::sync::mpsc;

use tiamat_core::ChunkPos;

use crate::mesher::{Mesh, MeshJob, Sight};
use crate::shade::BlockLight;
use crate::world::{ABSENT_POLICY, MeshInput};

/// Daylight everywhere, for the lighting mode that takes none from the
/// server.
const FLAT_DAYLIGHT: crate::shade::Uniform =
    crate::shade::Uniform(tiamat_core::light::Light::DAYLIGHT);

/// One chunk to mesh, with everything the mesher reads taken from the store
/// on the frame thread.
pub struct Job {
    /// Which chunk.
    pub pos: ChunkPos,
    /// The store's generation for it when the snapshot was taken.
    pub generation: u64,
    /// The snapshot.
    pub input: MeshInput,
    /// Whether to mesh against flat daylight rather than the stored light.
    pub flat: bool,
    /// What can be seen through, as the material table said.
    pub sight: Arc<Sight>,
}

/// A finished mesh, back on the frame thread.
pub struct Done {
    /// Which chunk.
    pub pos: ChunkPos,
    /// The generation the job was taken at — see the module's note on
    /// staleness.
    pub generation: u64,
    /// The mesh.
    pub mesh: Mesh,
}

/// The pool of mesh workers, and the two channels through it.
pub struct MeshPool {
    submit: Option<mpsc::Sender<Job>>,
    results: mpsc::Receiver<Done>,
    workers: Vec<std::thread::JoinHandle<()>>,
    /// Jobs submitted and not yet drained: what bounds the submissions a
    /// frame makes, so the queue never grows past what the workers can take
    /// up before the camera has moved on.
    backlog: usize,
    /// Threads in the pool; zero means jobs are run on the submitting
    /// thread, which is what a machine with no thread to spare gets and
    /// what a test that wants a mesh now asks for.
    threads: usize,
    /// What a threadless pool finished on `submit`, for the next `drain`.
    inline: Vec<Done>,
}

impl MeshPool {
    /// How many workers a machine gets: two cores kept for the frame thread
    /// and the embedded server, the rest up to six.
    #[must_use]
    pub fn default_workers() -> usize {
        std::thread::available_parallelism()
            .map_or(1, |cores| cores.get().saturating_sub(2).clamp(1, 6))
    }

    /// Starts `threads` workers. Zero runs every job inline on `submit`.
    #[must_use]
    pub fn new(threads: usize) -> Self {
        let (submit, jobs) = mpsc::channel::<Job>();
        let (done, results) = mpsc::channel::<Done>();
        let jobs = Arc::new(std::sync::Mutex::new(jobs));
        let mut workers = Vec::with_capacity(threads);
        for index in 0..threads {
            let jobs = Arc::clone(&jobs);
            let done = done.clone();
            let spawned = std::thread::Builder::new()
                .name(format!("mesh-{index}"))
                .spawn(move || {
                    loop {
                        // One receiver shared by every worker: whichever is
                        // free takes the next job.
                        let job = {
                            let Ok(jobs) = jobs.lock() else {
                                return;
                            };
                            jobs.recv()
                        };
                        let Ok(job) = job else {
                            return;
                        };
                        if done.send(work(job)).is_err() {
                            return;
                        }
                    }
                });
            match spawned {
                Ok(handle) => workers.push(handle),
                Err(err) => tracing::warn!("could not start a mesh worker: {err}"),
            }
        }
        let threads = workers.len();
        Self {
            submit: Some(submit),
            results,
            workers,
            backlog: 0,
            threads,
            inline: Vec::new(),
        }
    }

    /// Threads in the pool.
    #[must_use]
    pub const fn threads(&self) -> usize {
        self.threads
    }

    /// How many jobs may be in the pool at once before a frame stops
    /// submitting: two per worker, so every worker has one waiting when it
    /// finishes and the queue still turns over within a frame or two.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.threads * 2 + 1
    }

    /// Jobs submitted and not yet drained.
    #[must_use]
    pub const fn backlog(&self) -> usize {
        self.backlog
    }

    /// Hands a job to the pool, or runs it now when the pool has no threads.
    pub fn submit(&mut self, job: Job) {
        self.backlog += 1;
        if self.threads == 0 {
            let done = work(job);
            self.backlog -= 1;
            self.inline.push(done);
            return;
        }
        if let Some(submit) = &self.submit
            && submit.send(job).is_err()
        {
            self.backlog -= 1;
            tracing::warn!("the mesh workers are gone; a chunk was not meshed");
        }
    }

    /// Everything the workers have finished since the last call.
    pub fn drain(&mut self) -> Vec<Done> {
        let mut finished: Vec<Done> = std::mem::take(&mut self.inline);
        finished.extend(self.results.try_iter());
        self.backlog = self.backlog.saturating_sub(finished.len());
        finished
    }
}

impl Drop for MeshPool {
    fn drop(&mut self) {
        drop(self.submit.take());
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

/// Meshes one job.
fn work(job: Job) -> Done {
    let Job {
        pos,
        generation,
        input,
        flat,
        sight,
    } = job;
    let neighbours = input.neighbours();
    let mesh = match MeshJob::start(
        &input.chunk,
        &neighbours,
        ABSENT_POLICY,
        &input,
        sight.as_ref(),
    ) {
        // Empty air: nothing to draw, and the empty mesh still has to be set
        // so a chunk that was emptied stops drawing what it was.
        None => Mesh::default(),
        Some(job) => {
            if flat {
                drive(job, &FLAT_DAYLIGHT)
            } else {
                drive(job, &input)
            }
        }
    };
    Done {
        pos,
        generation,
        mesh,
    }
}

/// Runs a job to the end.
pub fn drive(mut job: MeshJob, light: &impl BlockLight) -> Mesh {
    while !job.step(light) {}
    job.finish(light)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::ChunkStore;
    use tiamat_core::{BlockPos, BlockValue, Chunk, MaterialId};

    fn store_with_a_floor() -> ChunkStore {
        let mut store = ChunkStore::new();
        let mut chunk = Chunk::new(ChunkPos::new(0, 0, 0), MaterialId::AIR);
        for x in 0..6 {
            for z in 0..6 {
                chunk
                    .set_block(BlockPos::new(x, 0, z), BlockValue::Uniform(MaterialId(2)))
                    .expect("in chunk");
            }
        }
        store.insert(chunk);
        store
    }

    fn job(store: &ChunkStore) -> Job {
        let pos = ChunkPos::new(0, 0, 0);
        Job {
            pos,
            generation: store.generation(pos),
            input: store.mesh_input(pos).expect("held"),
            flat: true,
            sight: Arc::new(Sight::default()),
        }
    }

    #[test]
    fn a_worker_meshes_a_snapshot_and_hands_it_back() {
        let store = store_with_a_floor();
        let mut pool = MeshPool::new(1);
        assert_eq!(pool.threads(), 1);
        pool.submit(job(&store));
        assert_eq!(pool.backlog(), 1);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let done = loop {
            let mut finished = pool.drain();
            if let Some(done) = finished.pop() {
                break done;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the worker never answered"
            );
            std::thread::yield_now();
        };
        assert_eq!(done.pos, ChunkPos::new(0, 0, 0));
        assert_eq!(done.generation, store.generation(done.pos));
        assert!(!done.mesh.quads.is_empty(), "a floor meshed to nothing");
        assert_eq!(pool.backlog(), 0);
    }

    #[test]
    fn a_threadless_pool_meshes_on_submit_and_answers_on_the_next_drain() {
        let store = store_with_a_floor();
        let mut pool = MeshPool::new(0);
        assert_eq!(pool.threads(), 0);
        pool.submit(job(&store));
        let finished = pool.drain();
        assert_eq!(finished.len(), 1);
        assert!(!finished[0].mesh.quads.is_empty());
        assert!(pool.drain().is_empty(), "handed back once");
    }

    #[test]
    fn a_snapshot_meshes_exactly_as_the_store_does() {
        // The owned input answers light, fluid and neighbours as the store's
        // own views do, so the mesh a worker makes is the mesh the frame
        // thread would have made.
        let store = store_with_a_floor();
        let pos = ChunkPos::new(0, 0, 0);
        let chunk = store.get(pos).expect("held");
        let on_frame = {
            let neighbours = store.neighbours(pos);
            let fluid = store.fluid_for(pos);
            let job = MeshJob::start(chunk, &neighbours, ABSENT_POLICY, &fluid, &Sight::default())
                .expect("not empty");
            drive(job, &FLAT_DAYLIGHT)
        };
        let on_worker = work(job(&store)).mesh;
        assert_eq!(on_frame.quads.len(), on_worker.quads.len());
        assert_eq!(on_frame.quads, on_worker.quads);
    }
}
