// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Packing the atlas off the frame thread (perf audit A1).
//!
//! # Why
//!
//! The material table and its textures arrive together, after the join, and
//! `App::adopt_atlas` packed them on the frame thread inside `pump_network`:
//! every decoded image resampled to a tile and blitted with its padding, the
//! mips built, and the alpha-tested tiles' coverage restored per level. For
//! a few hundred textures that is tens of milliseconds in one frame, at
//! exactly the moment the world appears. The work is pure — a table and a
//! map of images in, an atlas out — so it runs on a thread of its own, and
//! the frame thread polls for the result, uploads it, and marks the chunks
//! dirty as it did before.
//!
//! # What the frame thread still does
//!
//! The upload, `Renderer::set_atlas`, which needs the device. Everything
//! that needs no GPU is in [`Built`], the mips included, so a renderer that
//! takes them ready-made has nothing left to compute at upload time.
//!
//! # Wiring
//!
//! `App::adopt_atlas` becomes: start an [`AtlasBuild`] from the event's
//! table and images, keep it on the `App`, and each frame `poll` it; on
//! `Some(built)` do what `adopt_atlas` did after `build_atlas` — count
//! `built.atlas.filled()`, warn on `built.textures < built.wanted`, keep
//! `built.tiles`, upload `built.atlas`, set the tints, mark every chunk
//! dirty. Nothing here touches the `App`; this is the half that can be
//! tested without one.

use std::collections::BTreeMap;

use tiamat_core::proto::MaterialDef;

use crate::texture::{Atlas, Image, TileMap};

/// A finished atlas and everything derived from it without a GPU.
#[derive(Debug, Clone)]
pub struct Built {
    /// The packed atlas, slot = material id.
    pub atlas: Atlas,
    /// Its mip chain, level 0 first — `Atlas::mips`, computed here rather
    /// than at upload.
    pub mips: Vec<Image>,
    /// Where each material sits, for the interface.
    pub tiles: TileMap,
    /// How many materials named a texture.
    pub wanted: usize,
    /// How many textures there were to pack: the ones that arrived and
    /// decoded. Fewer than `wanted` is a server that offered more than
    /// arrived, and worth a warning.
    pub textures: usize,
}

/// Builds everything [`Built`] holds, on the calling thread.
#[must_use]
pub fn build(table: &[MaterialDef], images: &BTreeMap<u16, Image>) -> Built {
    let atlas = Atlas::for_table(table, images);
    let mips = atlas.mips();
    let tiles = atlas.tiles_only();
    Built {
        wanted: table.iter().filter(|entry| entry.texture.is_some()).count(),
        textures: images.len(),
        atlas,
        mips,
        tiles,
    }
}

/// An atlas being packed on its own thread.
///
/// Answers once, through [`AtlasBuild::poll`] or [`AtlasBuild::wait`];
/// after that there is nothing here but a value to drop.
pub struct AtlasBuild {
    thread: Option<std::thread::JoinHandle<Built>>,
    /// The answer, when no thread could be started and it was built here.
    inline: Option<Built>,
}

impl AtlasBuild {
    /// Starts packing `images` for `table` and returns at once.
    ///
    /// If no thread can be started the atlas is built here and now instead
    /// — what every join did before: slower, never wrong.
    #[must_use]
    pub fn start(table: Vec<MaterialDef>, images: BTreeMap<u16, Image>) -> Self {
        // Shared rather than cloned into the closure, so a thread that could
        // not start leaves the table and images here to build from.
        let input = std::sync::Arc::new((table, images));
        let on_thread = std::sync::Arc::clone(&input);
        let spawned = std::thread::Builder::new()
            .name("tiamat-atlas".to_owned())
            .spawn(move || build_isolated(&on_thread.0, &on_thread.1));
        match spawned {
            Ok(thread) => Self {
                thread: Some(thread),
                inline: None,
            },
            Err(err) => {
                tracing::warn!("could not start the atlas worker; packing on this thread: {err}");
                Self {
                    thread: None,
                    inline: Some(build_isolated(&input.0, &input.1)),
                }
            }
        }
    }

    /// Whether the answer is ready to take.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.inline.is_some()
            || self
                .thread
                .as_ref()
                .is_some_and(std::thread::JoinHandle::is_finished)
    }

    /// The atlas, once the worker has finished it.
    ///
    /// Non-blocking: for a loop that draws between calls. Answers once.
    pub fn poll(&mut self) -> Option<Built> {
        if let Some(built) = self.inline.take() {
            return Some(built);
        }
        if self.is_finished() {
            return self.thread.take().map(join_or_empty);
        }
        None
    }

    /// Blocks for the atlas. `None` only if it was already answered.
    #[must_use]
    pub fn wait(mut self) -> Option<Built> {
        self.inline
            .take()
            .or_else(|| self.thread.take().map(join_or_empty))
    }
}

impl std::fmt::Debug for AtlasBuild {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AtlasBuild")
            .field("finished", &self.is_finished())
            .finish_non_exhaustive()
    }
}

/// Builds, and answers with the all-chequer atlas if the build panics.
///
/// The images were decoded under the texture caps and the table is the
/// server's word; nothing here should panic, and if it does the join must
/// not go with it (charter rule 14): every material draws as the
/// missing-texture chequer, which is loud, until the table is sent again.
fn build_isolated(table: &[MaterialDef], images: &BTreeMap<u16, Image>) -> Built {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| build(table, images))).unwrap_or_else(
        |_| {
            tracing::error!(
                "packing the atlas panicked; every material draws as the missing-texture chequer"
            );
            build(table, &BTreeMap::new())
        },
    )
}

/// The worker's answer, or an empty atlas if the worker is gone.
fn join_or_empty(thread: std::thread::JoinHandle<Built>) -> Built {
    thread.join().unwrap_or_else(|_| {
        tracing::error!("the atlas worker panicked");
        build(&[], &BTreeMap::new())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn material(id: u16, textured: bool, cutout: bool) -> MaterialDef {
        MaterialDef {
            id,
            name: format!("test:{id}"),
            texture: textured.then_some([u8::try_from(id).expect("a small id"); 32]),
            step_sound: None,
            placeable: true,
            transparent: false,
            cutout,
            passable: false,
            sway: false,
            billboard: false,
            billboard_cross: false,
            friction: 1.0,
            tint: None,
            whole: false,
            model: None,
            flicker: None,
        }
    }

    /// Air, a textured block, a block whose texture never arrived, and a
    /// textured cutout.
    fn fixture() -> (Vec<MaterialDef>, BTreeMap<u16, Image>) {
        let table = vec![
            material(0, false, false),
            material(1, true, false),
            material(2, true, false),
            material(3, true, true),
        ];
        let mut images = BTreeMap::new();
        images.insert(1, Image::solid(16, 16, [200, 30, 30, 255]));
        images.insert(3, Image::tinted_with_border([30, 200, 30]));
        (table, images)
    }

    #[test]
    fn a_worker_packs_what_the_frame_thread_would() {
        let (table, images) = fixture();
        let here = build(&table, &images);
        let there = AtlasBuild::start(table, images)
            .wait()
            .expect("answers once");
        assert_eq!(there.atlas.image, here.atlas.image);
        assert_eq!(there.mips, here.mips);
        assert_eq!(there.tiles, here.tiles);
        assert_eq!(there.atlas.filled(), here.atlas.filled());
        assert_eq!(there.atlas.filled(), 2);
        assert_eq!((there.wanted, there.textures), (3, 2));
        assert_eq!(
            there.atlas.tile_uv(there.atlas.slot_of(3)),
            here.atlas.tile_uv(here.atlas.slot_of(3))
        );
    }

    #[test]
    fn poll_answers_once_the_worker_is_done_and_only_once() {
        let (table, images) = fixture();
        let mut pending = AtlasBuild::start(table, images);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let built = loop {
            if let Some(built) = pending.poll() {
                break built;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the worker never answered"
            );
            std::thread::yield_now();
        };
        assert_eq!(built.atlas.filled(), 2);
        assert!(pending.poll().is_none(), "answered once");
        assert!(pending.wait().is_none(), "and not again");
    }

    #[test]
    fn a_table_with_no_textures_still_builds() {
        let (table, _) = fixture();
        let built = AtlasBuild::start(table, BTreeMap::new())
            .wait()
            .expect("answers once");
        assert_eq!(built.atlas.filled(), 0);
        assert_eq!((built.wanted, built.textures), (3, 0));
        assert_eq!(built.mips.len(), built.atlas.mip_levels() as usize);
    }
}
