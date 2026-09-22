// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Unpacking a release archive, which is a hostile parser.
//!
//! **Charter rule 14, and the most dangerous parser in the project.** Every
//! other decoder here turns somebody's bytes into a picture or a sound; this
//! one turns them into files on disk that will then be executed. The archive
//! has already been checked against a signed BLAKE3 hash before this is
//! called, so in the ordinary case it is our own file — and this is written as
//! though it never is, because a check that only holds when nothing is wrong
//! is not a check.
//!
//! What it refuses, all of it by construction rather than by inspection:
//!
//! - **An absolute path or one containing `..`.** The classic escape, and the
//!   reason unpacking is done a component at a time rather than by handing a
//!   path to the filesystem and hoping.
//! - **A symlink or a hard link, of any shape.** A release contains neither,
//!   and a link is how an archive writes outside the directory it is unpacked
//!   into even when every path in it looks innocent.
//! - **Anything that is not a regular file or a directory** — devices, fifos,
//!   sockets.
//! - **More entries than [`MAX_ENTRIES`] or more bytes than
//!   [`MAX_UNPACKED_BYTES`]**, counted as they are written rather than trusted
//!   from a header, which is what makes a decompression bomb a refusal rather
//!   than a full disk.
//!
//! The fuzz target is `fuzz/fuzz_targets/archive_ingest.rs`, added with this
//! module rather than deferred to hardening.

use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

/// The most files one release archive may hold.
///
/// A release is a few hundred files — binaries, mods, textures, sounds. Ten
/// thousand is far past that and far below anything that takes time to refuse.
pub const MAX_ENTRIES: usize = 10_000;

/// The most a release may unpack to, in bytes.
///
/// The Linux archive is 18 MB compressed and about 50 MB unpacked. A gibibyte
/// is room for mods and art to grow by an order of magnitude and still refuses
/// a bomb long before a disk fills.
pub const MAX_UNPACKED_BYTES: u64 = 1024 * 1024 * 1024;

/// The most one file inside an archive may be.
pub const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;

/// Why an archive was refused.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    /// The gzip or tar framing did not read.
    #[error("the archive could not be read: {0}")]
    Unreadable(String),
    /// A path that would write outside the destination.
    #[error("the archive holds `{path}`, which is not a path inside it")]
    Escape {
        /// What it tried to write.
        path: String,
    },
    /// A link, a device, or anything else that is not a file or a directory.
    #[error("the archive holds `{path}`, which is not a file or a directory")]
    NotAFile {
        /// What it tried to write.
        path: String,
    },
    /// Over one of the documented caps.
    #[error("{what}")]
    TooBig {
        /// Which cap, and what it was.
        what: String,
    },
    /// The filesystem refused.
    #[error("cannot write {path}: {source}")]
    Io {
        /// Where.
        path: String,
        /// Why.
        source: std::io::Error,
    },
}

/// What came out of an archive.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Unpacked {
    /// How many files were written.
    pub files: usize,
    /// How many directories were made.
    pub directories: usize,
    /// How many bytes were written.
    pub bytes: u64,
}

/// Unpacks a gzipped tar into `into`, which must already exist and be empty.
///
/// # Errors
///
/// [`ArchiveError`] for anything in the module documentation: an escaping
/// path, a link, an oversized archive, or unreadable framing.
pub fn unpack_tar_gz(archive: &[u8], into: &Path) -> Result<Unpacked, ArchiveError> {
    let decoder = flate2::read::GzDecoder::new(archive);
    // **The reader is capped, not the header.** A gzip header can claim any
    // size at all, so the limit is applied to what actually comes out.
    let mut reader = tar::Archive::new(Capped {
        inner: decoder,
        left: MAX_UNPACKED_BYTES,
    });
    let entries = reader
        .entries()
        .map_err(|err| ArchiveError::Unreadable(err.to_string()))?;

    let mut out = Unpacked::default();
    for entry in entries {
        let mut entry = entry.map_err(|err| ArchiveError::Unreadable(err.to_string()))?;
        if out.files + out.directories >= MAX_ENTRIES {
            return Err(ArchiveError::TooBig {
                what: format!("the archive holds more than {MAX_ENTRIES} entries"),
            });
        }

        let path = entry
            .path()
            .map_err(|err| ArchiveError::Unreadable(err.to_string()))?
            .into_owned();
        let shown = path.display().to_string();
        let relative = safe_path(&path).ok_or(ArchiveError::Escape {
            path: shown.clone(),
        })?;
        let target = into.join(&relative);

        let kind = entry.header().entry_type();
        if kind.is_dir() {
            create_dir_all(&target)?;
            out.directories += 1;
            continue;
        }
        if !kind.is_file() {
            // Symlinks, hard links, devices, fifos. A release has none of
            // these, and a link is how an archive writes outside its own tree
            // while every path in it looks harmless.
            return Err(ArchiveError::NotAFile { path: shown });
        }

        let size = entry.header().size().unwrap_or(0);
        if size > MAX_FILE_BYTES {
            return Err(ArchiveError::TooBig {
                what: format!("`{shown}` claims {size} bytes, over the {MAX_FILE_BYTES} limit"),
            });
        }
        if let Some(parent) = target.parent() {
            create_dir_all(parent)?;
        }

        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .map_err(|err| ArchiveError::Unreadable(err.to_string()))?;
        out.bytes = out.bytes.saturating_add(bytes.len() as u64);
        if out.bytes > MAX_UNPACKED_BYTES {
            return Err(ArchiveError::TooBig {
                what: format!("the archive unpacks to more than {MAX_UNPACKED_BYTES} bytes"),
            });
        }
        std::fs::write(&target, &bytes).map_err(|source| ArchiveError::Io {
            path: target.display().to_string(),
            source,
        })?;
        // **The executable bit, and only that bit.** A release contains
        // programs, and a tar that could set any mode it liked could set
        // setuid. Whether the archive says 0755 or 0644 is all that is read.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = entry.header().mode().unwrap_or(0o644);
            let permissions = if mode & 0o111 == 0 { 0o644 } else { 0o755 };
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(permissions))
                .map_err(|source| ArchiveError::Io {
                    path: target.display().to_string(),
                    source,
                })?;
        }
        out.files += 1;
    }
    Ok(out)
}

/// A relative path with nothing clever in it, or nothing.
///
/// Built component by component rather than by cleaning a string: a path is
/// checked by the same rules the filesystem will apply, and `..` is refused
/// outright rather than resolved, because resolving is what lets `a/../../b`
/// look like `b` while landing somewhere else.
fn safe_path(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                // A Windows drive letter or a stream name arriving inside a
                // name, and anything with a separator hidden in it.
                let text = part.to_str()?;
                if text.is_empty() || text.contains(':') || text == "." || text == ".." {
                    return None;
                }
                out.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

fn create_dir_all(path: &Path) -> Result<(), ArchiveError> {
    std::fs::create_dir_all(path).map_err(|source| ArchiveError::Io {
        path: path.display().to_string(),
        source,
    })
}

/// A reader that refuses to produce more than it was told to.
///
/// The decompression bomb's answer: the limit is on what comes OUT, so a
/// kilobyte that claims to be a terabyte stops at the cap rather than at the
/// end of the disk.
struct Capped<R> {
    inner: R,
    left: u64,
}

impl<R: std::io::Read> std::io::Read for Capped<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.left == 0 {
            return Err(std::io::Error::other(format!(
                "the archive unpacks to more than {MAX_UNPACKED_BYTES} bytes"
            )));
        }
        let wanted = buf
            .len()
            .min(usize::try_from(self.left).unwrap_or(usize::MAX));
        let read = self.inner.read(&mut buf[..wanted])?;
        self.left -= read as u64;
        Ok(read)
    }
}

#[cfg(test)]
mod tests;
