// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only
//
//! Regenerates the game's icons from the designer's original.
//!
//! ```text
//! cargo run -p client --example make_icons -- assets/icon/tiamat-source.png assets/icon
//! ```
//!
//! Reads the square source picture, turns the white outside the disc clear,
//! and writes the three files the build embeds: `tiamat.png` (256 px, the
//! window icon), `tiamat.ico` (16 to 256 px, PNG entries, for the Windows
//! executables) and `tiamat.icns` (16 to 1024 px, for `Tiamat.app`).
//!
//! Downsampling is area-weighted on premultiplied colour, so the clear edge
//! never bleeds white into the crest. The containers are written by hand;
//! both are a header and a list of PNGs.

// A build tool for pictures, not simulation: `floor`, `ceil` and `round` are exact,
// and the lint guards the deterministic float subset (charter rule 4 "Scope").
#![allow(clippy::disallowed_methods)]

use std::collections::BTreeMap;
use std::error::Error;
use std::path::Path;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// Decodes the source into premultiplied RGBA floats; returns (side, pixels).
fn read(path: &Path) -> Result<(usize, Vec<[f32; 4]>)> {
    let mut decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path)?));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info()?;
    let (color, depth) = reader.output_color_type();
    let mut buf = vec![0; reader.output_buffer_size().ok_or("picture too large")?];
    let info = reader.next_frame(&mut buf)?;
    let (w, h) = (info.width as usize, info.height as usize);
    if w != h {
        return Err(format!("the source must be square, not {w}x{h}").into());
    }
    let ch = match color {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        other => return Err(format!("unsupported colour type {other:?}").into()),
    };
    if depth != png::BitDepth::Eight {
        return Err("the source must be 8 bits per channel".into());
    }
    let mut px = Vec::with_capacity(w * h);
    for p in buf.chunks_exact(ch).take(w * h) {
        let a = if ch == 4 { p[3] } else { 255 };
        // The disc sits on white; white is not part of the crest, so it becomes clear.
        let white = p[0] > 245 && p[1] > 245 && p[2] > 245;
        let a = if white { 0.0 } else { f32::from(a) / 255.0 };
        px.push([
            f32::from(p[0]) / 255.0 * a,
            f32::from(p[1]) / 255.0 * a,
            f32::from(p[2]) / 255.0 * a,
            a,
        ]);
    }
    Ok((w, px))
}

/// Area-weighted resample: every source pixel counts by how much of it a
/// target pixel covers. Returns straight (un-premultiplied) RGBA bytes.
fn resize(src: &[[f32; 4]], n: usize, m: usize) -> Vec<u8> {
    let scale = n as f64 / m as f64;
    let mut out = Vec::with_capacity(m * m * 4);
    for ty in 0..m {
        let (y0, y1) = (ty as f64 * scale, (ty + 1) as f64 * scale);
        for tx in 0..m {
            let (x0, x1) = (tx as f64 * scale, (tx + 1) as f64 * scale);
            let mut acc = [0f64; 4];
            let mut wsum = 0.0;
            for sy in y0.floor() as usize..(y1.ceil() as usize).min(n) {
                let wy = (y1.min(sy as f64 + 1.0) - y0.max(sy as f64)).max(0.0);
                for sx in x0.floor() as usize..(x1.ceil() as usize).min(n) {
                    let wx = (x1.min(sx as f64 + 1.0) - x0.max(sx as f64)).max(0.0);
                    let w = wx * wy;
                    let p = src[sy * n + sx];
                    for (c, a) in acc.iter_mut().enumerate() {
                        *a += f64::from(p[c]) * w;
                    }
                    wsum += w;
                }
            }
            let a = acc[3] / wsum;
            for c in acc.iter().take(3) {
                let v = if a > 0.0 { c / wsum / a } else { 0.0 };
                out.push((v * 255.0).round().clamp(0.0, 255.0) as u8);
            }
            out.push((a * 255.0).round().clamp(0.0, 255.0) as u8);
        }
    }
    out
}

/// Encodes straight RGBA bytes as an `m` by `m` PNG.
fn encode(rgba: &[u8], m: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut enc = png::Encoder::new(&mut bytes, u32::try_from(m)?, u32::try_from(m)?);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::High);
    let mut w = enc.write_header()?;
    w.write_image_data(rgba)?;
    w.finish()?;
    Ok(bytes)
}

/// `.ico`: ICONDIR, one ICONDIRENTRY per size, then PNG payloads (Vista and later).
fn ico(pngs: &BTreeMap<usize, Vec<u8>>, sizes: &[usize]) -> Result<Vec<u8>> {
    let mut out = vec![0, 0, 1, 0];
    out.extend_from_slice(&u16::try_from(sizes.len())?.to_le_bytes());
    let mut offset = 6 + 16 * sizes.len();
    for s in sizes {
        let len = pngs[s].len();
        // A dimension byte of 0 means 256.
        let d = if *s >= 256 { 0u8 } else { u8::try_from(*s)? };
        out.extend_from_slice(&[d, d, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&u32::try_from(len)?.to_le_bytes());
        out.extend_from_slice(&u32::try_from(offset)?.to_le_bytes());
        offset += len;
    }
    for s in sizes {
        out.extend_from_slice(&pngs[s]);
    }
    Ok(out)
}

/// `.icns`: `icns` and a length, then (type, length including header, PNG) chunks.
fn icns(pngs: &BTreeMap<usize, Vec<u8>>) -> Result<Vec<u8>> {
    let entries: [(&[u8; 4], usize); 11] = [
        (b"icp4", 16),
        (b"icp5", 32),
        (b"icp6", 64),
        (b"ic07", 128),
        (b"ic08", 256),
        (b"ic09", 512),
        (b"ic10", 1024),
        (b"ic11", 32),
        (b"ic12", 64),
        (b"ic13", 256),
        (b"ic14", 512),
    ];
    let mut body = Vec::new();
    for (kind, s) in entries {
        body.extend_from_slice(kind);
        body.extend_from_slice(&u32::try_from(pngs[&s].len() + 8)?.to_be_bytes());
        body.extend_from_slice(&pngs[&s]);
    }
    let mut out = b"icns".to_vec();
    out.extend_from_slice(&u32::try_from(body.len() + 8)?.to_be_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (Some(src), Some(out)) = (args.get(1), args.get(2)) else {
        return Err("usage: make_icons <source.png> <output dir>".into());
    };
    let out = Path::new(out);
    std::fs::create_dir_all(out)?;
    let (n, px) = read(Path::new(src))?;
    let mut pngs = BTreeMap::new();
    for m in [16usize, 24, 32, 48, 64, 128, 256, 512, 1024] {
        pngs.insert(m, encode(&resize(&px, n, m), m)?);
    }
    std::fs::write(out.join("tiamat.png"), &pngs[&256])?;
    let ico = ico(&pngs, &[16, 24, 32, 48, 64, 128, 256])?;
    std::fs::write(out.join("tiamat.ico"), &ico)?;
    let icns = icns(&pngs)?;
    std::fs::write(out.join("tiamat.icns"), &icns)?;
    println!(
        "tiamat.png {} bytes, tiamat.ico {} bytes, tiamat.icns {} bytes",
        pngs[&256].len(),
        ico.len(),
        icns.len()
    );
    Ok(())
}
