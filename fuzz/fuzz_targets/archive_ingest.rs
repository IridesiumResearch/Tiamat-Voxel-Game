// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Fuzzes the release archive extractor — charter rule 14, and the most
//! dangerous parser in the project.
//!
//! Every other decoder turns somebody's bytes into a picture or a sound. This
//! one turns them into FILES, in a directory whose contents are then executed.
//! The archive has been checked against a signed BLAKE3 hash before the
//! extractor sees it in the ordinary case — and this target exists because a
//! check that only holds when nothing is wrong is not a check.
//!
//! # What is being tested
//!
//! Not that an archive unpacks: almost every input here is nonsense and should
//! be refused. Three things:
//!
//! 1. **A refusal is orderly** — an error, not a panic and not an abort.
//! 2. **Nothing is written outside the destination.** The target unpacks into
//!    a fresh directory that sits inside a sentinel one, and afterwards the
//!    sentinel must hold nothing but that directory. A traversal that gets
//!    past the path rules would land there and be caught.
//! 3. **The caps hold**: whatever comes out is within the documented limits,
//!    so a bomb is refused rather than filling a disk.

#![no_main]

use libfuzzer_sys::fuzz_target;
use updater::archive::{MAX_ENTRIES, MAX_UNPACKED_BYTES, unpack_tar_gz};

fuzz_target!(|data: &[u8]| {
    // A sentinel directory with exactly one thing in it. Anything that
    // escapes the destination lands here and is counted afterwards.
    let sentinel = std::env::temp_dir().join("tiamot-fuzz-archive");
    let _ = std::fs::remove_dir_all(&sentinel);
    let into = sentinel.join("into");
    if std::fs::create_dir_all(&into).is_err() {
        return;
    }

    if let Ok(unpacked) = unpack_tar_gz(data, &into) {
        assert!(
            unpacked.bytes <= MAX_UNPACKED_BYTES,
            "unpacked {} bytes, over the cap",
            unpacked.bytes
        );
        assert!(
            unpacked.files + unpacked.directories <= MAX_ENTRIES,
            "unpacked {} entries, over the cap",
            unpacked.files + unpacked.directories
        );
    }

    // Whatever happened, only `into` may exist under the sentinel.
    let strays: Vec<String> = std::fs::read_dir(&sentinel)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name != "into")
        .collect();
    assert!(strays.is_empty(), "the archive wrote outside it: {strays:?}");

    let _ = std::fs::remove_dir_all(&sentinel);
});
