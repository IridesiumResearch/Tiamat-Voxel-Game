// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Writes the reference mod's model-block fixture, `core:brazier`.
//!
//! Run: `cargo run -p tiamat-core --example block_model_fixture -- game/core_blocks/models`
//!
//! A brazier: a slab for a foot, a stem, and a bowl — three cuboids with
//! nothing a cube of cells could be mistaken for, so a screenshot that shows
//! it has shown a model and not a block. Built from [`tiamat_core::model`]'s
//! own types and written with [`tiamat_core::model::build::to_glb`], the same
//! encoder the fuzz seeds use, so the fixture is a `.glb` the engine's reader
//! is known to accept and nobody has to open a modelling program to change a
//! reference mod.
//!
//! Model space is cells (Sub-Node Contract §8.6): three units to the block,
//! the origin at the block's bottom centre, so the brazier spans x and z from
//! -1.5 to 1.5 and y from 0 to 3. Its `shape` in `core_blocks/init.lua` is the
//! bottom layer, the centre cell of the middle, and the full top layer — what
//! the world knows of it — and the model is drawn over that.

use std::path::PathBuf;

use tiamat_core::model::{Model, Vertex, build};

/// An axis-aligned box from `min` to `max`, six faces, outward normals.
fn cuboid(model: &mut Model, min: [f32; 3], max: [f32; 3]) {
    // Each face: normal, then the four corners counter-clockwise seen from
    // outside, as (axis, which end, the two spanning axes).
    let faces: [([f32; 3], [[f32; 3]; 4]); 6] = [
        // +Y (top)
        (
            [0.0, 1.0, 0.0],
            [
                [min[0], max[1], min[2]],
                [min[0], max[1], max[2]],
                [max[0], max[1], max[2]],
                [max[0], max[1], min[2]],
            ],
        ),
        // -Y (bottom)
        (
            [0.0, -1.0, 0.0],
            [
                [min[0], min[1], min[2]],
                [max[0], min[1], min[2]],
                [max[0], min[1], max[2]],
                [min[0], min[1], max[2]],
            ],
        ),
        // +X
        (
            [1.0, 0.0, 0.0],
            [
                [max[0], min[1], min[2]],
                [max[0], max[1], min[2]],
                [max[0], max[1], max[2]],
                [max[0], min[1], max[2]],
            ],
        ),
        // -X
        (
            [-1.0, 0.0, 0.0],
            [
                [min[0], min[1], max[2]],
                [min[0], max[1], max[2]],
                [min[0], max[1], min[2]],
                [min[0], min[1], min[2]],
            ],
        ),
        // +Z
        (
            [0.0, 0.0, 1.0],
            [
                [max[0], min[1], max[2]],
                [max[0], max[1], max[2]],
                [min[0], max[1], max[2]],
                [min[0], min[1], max[2]],
            ],
        ),
        // -Z
        (
            [0.0, 0.0, -1.0],
            [
                [min[0], min[1], min[2]],
                [min[0], max[1], min[2]],
                [max[0], max[1], min[2]],
                [max[0], min[1], min[2]],
            ],
        ),
    ];
    let uvs = [[0.0, 1.0], [0.0, 0.0], [1.0, 0.0], [1.0, 1.0]];
    for (normal, corners) in faces {
        let base = model.vertices.len() as u32;
        for (corner, uv) in corners.iter().zip(uvs) {
            model.vertices.push(Vertex {
                position: *corner,
                normal,
                uv,
                // A static mesh: every influence is the identity joint.
                joints: [0; 4],
                weights: [1.0, 0.0, 0.0, 0.0],
            });
        }
        model
            .indices
            .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

/// The brazier, in cells.
fn brazier() -> Model {
    let mut model = Model::default();
    // The foot: a slab across the whole block, half a cell thick.
    cuboid(&mut model, [-1.5, 0.0, -1.5], [1.5, 0.5, 1.5]);
    // The stem: the centre cell, from the slab to the bowl.
    cuboid(&mut model, [-0.5, 0.5, -0.5], [0.5, 2.0, 0.5]);
    // The bowl: wider than the stem, narrower than the block, a cell deep.
    cuboid(&mut model, [-1.2, 2.0, -1.2], [1.2, 3.0, 1.2]);
    model
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .map_or_else(|| PathBuf::from("game/core_blocks/models"), PathBuf::from);
    if let Err(err) = std::fs::create_dir_all(&out) {
        eprintln!("could not create `{}`: {err}", out.display());
        std::process::exit(1);
    }
    let bytes = build::to_glb(&brazier());
    // Read back with the reader a client uses, so the fixture is known good
    // before it is committed.
    if let Err(err) = tiamat_core::model::load(&bytes, &tiamat_core::model::Limits::default()) {
        eprintln!("the brazier does not load: {err}");
        std::process::exit(1);
    }
    let path = out.join("brazier.glb");
    if let Err(err) = std::fs::write(&path, &bytes) {
        eprintln!("could not write `{}`: {err}", path.display());
        std::process::exit(1);
    }
    println!("wrote {} ({} bytes)", path.display(), bytes.len());
}
