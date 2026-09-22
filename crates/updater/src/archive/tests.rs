// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! What an archive must not be allowed to do.
//!
//! Every test here builds a malicious tar by hand and points it at a scratch
//! directory with a witness file beside it. The witness is the point: an
//! escape that is refused leaves it alone, and a test that only checked for an
//! error could pass while the file was overwritten first.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use super::*;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamot-archive-tests").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

/// A gzipped tar built from `(path, kind, bytes)` entries.
fn tar_gz(entries: &[(&str, tar::EntryType, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (path, kind, bytes) in entries {
        let mut header = tar::Header::new_gnu();
        // `set_path` refuses some of the paths these tests are about, so the
        // name goes in raw — which is also how a hostile archive would carry
        // it. Long names are not needed here.
        header
            .as_gnu_mut()
            .expect("a gnu header")
            .name
            .get_mut(..path.len())
            .expect("the fixtures use short names")
            .copy_from_slice(path.as_bytes());
        header.set_size(bytes.len() as u64);
        header.set_entry_type(*kind);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append(&header, std::io::Cursor::new(*bytes))
            .expect("append");
    }
    let tar = builder.into_inner().expect("finish");
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&tar).expect("compress");
    encoder.finish().expect("finish")
}

/// The witness: a file beside the destination that nothing may touch.
fn witness(root: &Path) -> PathBuf {
    let path = root.join("witness.txt");
    std::fs::write(&path, b"untouched").expect("witness");
    path
}

#[test]
fn an_ordinary_archive_unpacks() {
    let root = scratch("ordinary");
    let into = root.join("into");
    std::fs::create_dir_all(&into).expect("into");
    let archive = tar_gz(&[
        ("current/", tar::EntryType::Directory, b""),
        ("current/client", tar::EntryType::Regular, b"a binary"),
        ("README.txt", tar::EntryType::Regular, b"hello"),
    ]);

    let out = unpack_tar_gz(&archive, &into).expect("an ordinary archive unpacks");
    assert_eq!(out.files, 2);
    assert_eq!(out.bytes, 13);
    assert_eq!(
        std::fs::read(into.join("current/client")).expect("written"),
        b"a binary"
    );
}

#[test]
fn a_path_that_climbs_out_is_refused_and_writes_nothing() {
    // The classic. `..` is refused rather than resolved, because resolving is
    // what lets `a/../../b` look like `b` and land somewhere else.
    let root = scratch("climb");
    let into = root.join("into");
    std::fs::create_dir_all(&into).expect("into");
    let witness = witness(&root);

    for path in [
        "../witness.txt",
        "current/../../witness.txt",
        "./../witness.txt",
    ] {
        let archive = tar_gz(&[(path, tar::EntryType::Regular, b"owned")]);
        let err = unpack_tar_gz(&archive, &into).expect_err(path);
        assert!(matches!(err, ArchiveError::Escape { .. }), "{path}: {err}");
    }
    assert_eq!(
        std::fs::read(&witness).expect("still there"),
        b"untouched",
        "an escaping archive wrote outside the destination"
    );
}

#[test]
fn an_absolute_path_is_refused() {
    let root = scratch("absolute");
    let into = root.join("into");
    std::fs::create_dir_all(&into).expect("into");
    let witness = witness(&root);

    // A leading slash, and the Windows shapes: a drive letter and a name with
    // a colon in it. None of them may become a path.
    for path in ["/etc/passwd", "C:/windows/system32/x", "current:stream"] {
        let archive = tar_gz(&[(path, tar::EntryType::Regular, b"owned")]);
        let err = unpack_tar_gz(&archive, &into).expect_err(path);
        assert!(matches!(err, ArchiveError::Escape { .. }), "{path}: {err}");
    }
    assert_eq!(std::fs::read(&witness).expect("still there"), b"untouched");
}

#[test]
fn a_link_of_any_kind_is_refused() {
    // A release contains no links. A link is how an archive writes outside its
    // own tree while every path in it looks harmless: unpack a symlink called
    // `current` pointing at somebody's home directory, then unpack
    // `current/anything` through it.
    let root = scratch("links");
    let into = root.join("into");
    std::fs::create_dir_all(&into).expect("into");

    for kind in [
        tar::EntryType::Symlink,
        tar::EntryType::Link,
        tar::EntryType::Fifo,
        tar::EntryType::Char,
        tar::EntryType::Block,
    ] {
        let archive = tar_gz(&[("current/client", kind, b"")]);
        let err = unpack_tar_gz(&archive, &into).expect_err("a link is not a file");
        assert!(
            matches!(err, ArchiveError::NotAFile { .. }),
            "{kind:?}: {err}"
        );
    }
}

#[test]
fn a_bomb_stops_at_the_cap_rather_than_at_the_disk() {
    // A gzip header can claim anything, so the limit is on what comes OUT.
    // Zeroes compress to almost nothing, which is what makes this cheap to
    // build and expensive to trust.
    let root = scratch("bomb");
    let into = root.join("into");
    std::fs::create_dir_all(&into).expect("into");

    let huge = vec![0_u8; 2 * 1024 * 1024];
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(huge.len() as u64);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_mode(0o644);
    header.set_path("big").expect("path");
    header.set_cksum();
    builder
        .append(&header, std::io::Cursor::new(&huge))
        .expect("append");
    let tar = builder.into_inner().expect("finish");
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    encoder.write_all(&tar).expect("compress");
    let archive = encoder.finish().expect("finish");

    // Two megabytes of zeroes is a couple of kilobytes on the wire: the ratio
    // a bomb is built from, in miniature.
    assert!(
        archive.len() < 64 * 1024,
        "the fixture is not compressed enough to be the test it claims: {}",
        archive.len()
    );

    // Under the cap it unpacks; the cap itself is checked by the unit test
    // below, because building a gibibyte here would cost more than it proves.
    let out = unpack_tar_gz(&archive, &into).expect("two megabytes is under the cap");
    assert_eq!(out.bytes, 2 * 1024 * 1024);
}

#[test]
fn the_caps_are_the_documented_ones() {
    // The reader that enforces the total, tested directly: a gibibyte of
    // fixture would cost a minute to prove what this proves in a millisecond.
    use std::io::Read as _;
    let mut capped = Capped {
        inner: std::io::repeat(0),
        left: 8,
    };
    let mut out = Vec::new();
    let err = capped
        .read_to_end(&mut out)
        .expect_err("a reader with no end must be stopped");
    assert_eq!(out.len(), 8, "it stopped exactly at the cap");
    assert!(err.to_string().contains("unpacks to more than"), "{err}");
}

#[test]
fn a_file_claiming_more_than_the_per_file_cap_is_refused() {
    let root = scratch("per-file");
    let into = root.join("into");
    std::fs::create_dir_all(&into).expect("into");

    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(MAX_FILE_BYTES + 1);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_mode(0o644);
    header.set_path("huge").expect("path");
    header.set_cksum();
    // The header lies about the size and the body is empty, which is exactly
    // what a handmade archive can do.
    builder
        .append(&header, std::io::Cursor::new(Vec::new()))
        .expect("append");
    let tar = builder.into_inner().expect("finish");
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&tar).expect("compress");
    let archive = encoder.finish().expect("finish");

    let err = unpack_tar_gz(&archive, &into).expect_err("a file over the cap");
    assert!(matches!(err, ArchiveError::TooBig { .. }), "{err}");
}

#[test]
fn rubbish_is_an_error_rather_than_a_panic() {
    let root = scratch("rubbish");
    let into = root.join("into");
    std::fs::create_dir_all(&into).expect("into");

    for bytes in [
        &b""[..],
        &b"not an archive"[..],
        &[0x1F, 0x8B, 0x08, 0x00][..], // a gzip header and nothing after it
        &[0xFF; 512][..],
    ] {
        let result = unpack_tar_gz(bytes, &into);
        assert!(result.is_err() || result.expect("checked").files == 0);
    }
}

#[cfg(unix)]
#[test]
fn only_the_executable_bit_survives_from_the_archive() {
    // A tar can ask for any mode, including setuid. A release needs exactly
    // one distinction: is this a program or is it data.
    use std::os::unix::fs::PermissionsExt as _;

    let root = scratch("modes");
    let into = root.join("into");
    std::fs::create_dir_all(&into).expect("into");

    let mut builder = tar::Builder::new(Vec::new());
    for (name, mode) in [("client", 0o4755), ("data", 0o666)] {
        let mut header = tar::Header::new_gnu();
        header.set_size(1);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(mode);
        header.set_path(name).expect("path");
        header.set_cksum();
        builder
            .append(&header, std::io::Cursor::new(b"x".as_slice()))
            .expect("append");
    }
    let tar = builder.into_inner().expect("finish");
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&tar).expect("compress");
    let archive = encoder.finish().expect("finish");

    unpack_tar_gz(&archive, &into).expect("unpacks");
    let mode_of = |name: &str| {
        std::fs::metadata(into.join(name))
            .expect("written")
            .permissions()
            .mode()
            & 0o7777
    };
    assert_eq!(mode_of("client"), 0o755, "setuid survived the unpack");
    assert_eq!(mode_of("data"), 0o644);
}
