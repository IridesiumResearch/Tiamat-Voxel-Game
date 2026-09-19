// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! What a density program near the cap costs per chunk — World ask 30.

use std::time::Instant;
use tiamot_core::detgen::{Axis, Density, Op, Region3d, UNSTRETCHED, default_params};

/// A program of roughly `ops` operations with `noises` noise reads in it,
/// shaped like a shore program: noise, arithmetic on it, folded together.
fn program(ops: usize, noises: usize) -> Vec<Op> {
    let mut out = vec![Op::Noise {
        params: default_params(),
        amplitude: 1.0,
        stream: 0,
        stretch: UNSTRETCHED,
    }];
    let mut stream = 1u64;
    while out.len() < ops {
        if out.len() % (ops / noises.max(1)) == 0 && stream < noises as u64 {
            out.push(Op::Noise {
                params: default_params(),
                amplitude: 1.0,
                stream,
                stretch: UNSTRETCHED,
            });
            stream += 1;
            out.push(Op::Add);
            continue;
        }
        out.push(Op::Coordinate(Axis::Y));
        out.push(Op::Constant(0.001));
        out.push(Op::Multiply);
        out.push(Op::Subtract);
    }
    out
}

fn main() {
    let region = Region3d {
        origin_x: 0.0,
        origin_y: 0.0,
        origin_z: 0.0,
        step: 1.0,
        width: 16,
        height: 16,
        depth: 16,
    };
    let mut out = vec![0.0f32; region.len()];
    for (ops, noises) in [
        (1000, 8),
        (1000, 16),
        (1000, 32),
        (4000, 8),
        (4000, 16),
        (4000, 32),
        (4000, 64),
    ] {
        let built = program(ops, noises);
        let len = built.len();
        let reads = built
            .iter()
            .filter(|op| matches!(op, Op::Noise { .. }))
            .count();
        let density = match Density::compile(built) {
            Ok(density) => density,
            Err(err) => {
                println!("{len} ops, {reads} noise reads: refused: {err}");
                continue;
            }
        };
        // Warm.
        density.evaluate(42, &region, &mut out).expect("evaluate");
        let start = Instant::now();
        const RUNS: u32 = 20;
        for _ in 0..RUNS {
            density.evaluate(42, &region, &mut out).expect("evaluate");
        }
        let each = start.elapsed().as_secs_f64() * 1000.0 / f64::from(RUNS);
        println!(
            "{len} ops, {reads} noise reads: {each:.3} ms a chunk, {:.1}% of a 50 ms tick",
            each / 50.0 * 100.0
        );
    }
}
