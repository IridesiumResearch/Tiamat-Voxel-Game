// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! `server --create-mod <id>`: a new mod from the template in `api/template/`.
//!
//! The template is compiled into this binary, so the command works from an
//! installed copy with no engine checkout beside it, and what it writes is
//! exactly what `api/template/` held at the commit the binary was built
//! from: the tour of the API in `init.lua`, the manifest, a texture, a sound,
//! the editor config, and the stubs and `AGENTS.md` vendored from `api/`,
//! which are MIT so that a mod of any licence may carry them.
//!
//! **The headers are rewritten.** The template's own files carry the `api/`
//! header (Iridesium, MIT) because that is where they live and what CI checks
//! them against; the mod they become belongs to whoever asked for it, so the
//! generated files name that holder and licence instead. The vendored stubs
//! and `AGENTS.md` keep theirs: they are still the engine's.
//!
//! **Checked before it is handed over.** A template that does not pass
//! `--check-mods` is a template with a bug, so the new mod is checked alone in
//! a scratch set — alone, so a broken neighbour in the destination cannot
//! fail a mod that is fine — and the command's exit code says what the check
//! said.

use std::path::{Path, PathBuf};

use tiamat_core::modload::is_valid_id;

use crate::checkmods::{self, CheckReport};

/// What to scaffold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMod {
    /// The mod id: lowercase letters, digits and underscores, starting with a
    /// letter. The namespace of everything the mod registers.
    pub id: String,
    /// The display name.
    pub name: String,
    /// The SPDX licence identifier the generated files carry.
    pub license: String,
    /// The copyright holder the generated files name.
    pub copyright: String,
}

impl NewMod {
    /// The defaults the command fills in: a name from the id (`my_mod` is
    /// "My Mod"), MIT, and "<name> authors" for the holder.
    #[must_use]
    pub fn with_defaults(
        id: &str,
        name: Option<&str>,
        license: Option<&str>,
        copyright: Option<&str>,
    ) -> Self {
        let name = name.map_or_else(|| title_of(id), str::to_owned);
        let copyright = copyright.map_or_else(|| format!("{name} authors"), str::to_owned);
        Self {
            id: id.to_owned(),
            name,
            license: license.unwrap_or("MIT").to_owned(),
            copyright,
        }
    }
}

/// `my_little_mod` as "My Little Mod".
fn title_of(id: &str) -> String {
    id.split('_')
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Why a mod could not be scaffolded.
#[derive(Debug, thiserror::Error)]
pub enum ScaffoldError {
    /// The id would be refused by the manifest loader.
    #[error(
        "`{0}` is not a mod id: lowercase letters, digits and underscores, starting with a letter"
    )]
    InvalidId(String),
    /// A field that goes into a header or the manifest is empty or has a
    /// newline or a quote in it.
    #[error("the {0} must be one line, not empty, with no double quote in it")]
    Blank(&'static str),
    /// The destination is taken. A template never overwrites.
    #[error("`{}` already exists; a template never overwrites", .0.display())]
    Exists(PathBuf),
    /// A file could not be written.
    #[error("cannot write `{path}`: {source}")]
    Io {
        /// What was being written.
        path: PathBuf,
        /// Why it failed.
        #[source]
        source: std::io::Error,
    },
}

/// One file of the template.
struct Piece {
    /// Where it goes, under the new mod's directory.
    path: &'static str,
    /// What it holds, at the commit this binary was built from.
    bytes: &'static [u8],
    /// Whether it is the template's own text, with placeholders to fill and a
    /// header to make the new mod's. The vendored files are copied as they
    /// are.
    text: bool,
}

const PIECES: &[Piece] = &[
    Piece {
        path: "mod.toml",
        bytes: include_bytes!("../../../api/template/mod.toml"),
        text: true,
    },
    Piece {
        path: "init.lua",
        bytes: include_bytes!("../../../api/template/init.lua"),
        text: true,
    },
    Piece {
        path: "README.md",
        bytes: include_bytes!("../../../api/template/README.md"),
        text: true,
    },
    Piece {
        path: ".luarc.json",
        bytes: include_bytes!("../../../api/template/.luarc.json"),
        text: true,
    },
    Piece {
        path: "textures/block.png",
        bytes: include_bytes!("../../../api/template/textures/block.png"),
        text: false,
    },
    Piece {
        path: "sounds/ping.wav",
        bytes: include_bytes!("../../../api/template/sounds/ping.wav"),
        text: false,
    },
    // Vendored from `api/` and left exactly as they are, header included: a
    // mod author updates them by copying the engine's newer ones over.
    Piece {
        path: "stubs/game.lua",
        bytes: include_bytes!("../../../api/stubs/game.lua"),
        text: false,
    },
    Piece {
        path: "AGENTS.md",
        bytes: include_bytes!("../../../api/AGENTS.md"),
        text: false,
    },
];

/// The template's own header, which the generated files replace with theirs.
const TEMPLATE_HOLDER: &str = "SPDX-FileCopyrightText: Iridesium";
const TEMPLATE_LICENSE: &str = "SPDX-License-Identifier: MIT";

/// Writes the new mod under `into`, and answers its directory.
///
/// # Errors
///
/// A bad id or field, a directory already there, or a write that failed —
/// see [`ScaffoldError`]. Nothing is written before the checks pass, and a
/// failed write leaves what it wrote for the caller to see.
pub fn create(spec: &NewMod, into: &Path) -> Result<PathBuf, ScaffoldError> {
    if !is_valid_id(&spec.id) {
        return Err(ScaffoldError::InvalidId(spec.id.clone()));
    }
    for (what, value) in [
        ("name", &spec.name),
        ("licence", &spec.license),
        ("copyright holder", &spec.copyright),
    ] {
        if value.trim().is_empty() || value.contains('\n') || value.contains('"') {
            return Err(ScaffoldError::Blank(what));
        }
    }
    let dir = into.join(&spec.id);
    if dir.exists() {
        return Err(ScaffoldError::Exists(dir));
    }
    for piece in PIECES {
        let path = dir.join(piece.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| ScaffoldError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let written = if piece.text {
            std::fs::write(&path, filled(piece.bytes, spec))
        } else {
            std::fs::write(&path, piece.bytes)
        };
        written.map_err(|source| ScaffoldError::Io { path, source })?;
    }
    Ok(dir)
}

/// The template's text with the placeholders filled and the header made the
/// new mod's.
///
/// **Line endings are the template's, not the checkout's.** The text is
/// embedded at build time from the engine's working copy, and a Windows
/// checkout with `core.autocrlf` hands the compiler CRLF — which would make
/// what `--create-mod` writes depend on how the engine was cloned, and made
/// the scaffold tests red on Windows alone. Normalised to LF here, once.
fn filled(bytes: &[u8], spec: &NewMod) -> String {
    String::from_utf8_lossy(bytes)
        .replace("\r\n", "\n")
        .replace("{{MOD_ID}}", &spec.id)
        .replace("{{MOD_NAME}}", &spec.name)
        .replace("{{LICENSE}}", &spec.license)
        .replace(
            TEMPLATE_HOLDER,
            &format!("SPDX-FileCopyrightText: {}", spec.copyright),
        )
        .replace(
            TEMPLATE_LICENSE,
            &format!("SPDX-License-Identifier: {}", spec.license),
        )
}

/// Checks a mod alone, the way `--check-mods` would, in a scratch set that
/// holds nothing else.
///
/// # Errors
///
/// The mod could not be copied out, or the set could not be resolved — the
/// same string `checkmods::check` answers with.
pub fn check_alone(dir: &Path) -> Result<CheckReport, String> {
    let Some(id) = dir.file_name() else {
        return Err(format!("`{}` has no name to check under", dir.display()));
    };
    let scratch = std::env::temp_dir().join(format!(
        "tiamat-create-mod-{}-{}",
        id.to_string_lossy(),
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&scratch);
    copy_tree(dir, &scratch.join(id))
        .map_err(|err| format!("cannot copy the mod out to check it: {err}"))?;
    let report = checkmods::check(&scratch);
    let _ = std::fs::remove_dir_all(&scratch);
    report
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("tiamat-scaffold").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn a_new_mod_from_the_template_checks_clean_and_registers_its_block() {
        // Task 16's acceptance: the template passes `--check-mods` and its
        // block is there, with nothing in the engine or `game/` edited.
        let into = scratch("demo");
        let spec = NewMod::with_defaults("demo", None, None, None);
        let dir = create(&spec, &into).expect("scaffold");
        assert_eq!(dir, into.join("demo"));
        for piece in PIECES {
            assert!(dir.join(piece.path).is_file(), "{} is missing", piece.path);
        }

        let init = std::fs::read_to_string(dir.join("init.lua")).expect("init.lua");
        assert!(
            init.starts_with(
                "-- SPDX-FileCopyrightText: Demo authors\n-- SPDX-License-Identifier: MIT\n"
            ),
            "the header is not the mod's own:\n{}",
            &init[..120]
        );
        for name in ["mod.toml", "init.lua", "README.md", ".luarc.json"] {
            let text = std::fs::read_to_string(dir.join(name)).expect(name);
            assert!(!text.contains("{{"), "{name} still has a placeholder");
        }
        let manifest = std::fs::read_to_string(dir.join("mod.toml")).expect("mod.toml");
        assert!(manifest.contains("id = \"demo\""), "{manifest}");
        assert!(manifest.contains("name = \"Demo\""), "{manifest}");
        assert!(manifest.contains("license = \"MIT\""), "{manifest}");
        // The vendored files are the engine's, header and all.
        let stubs = std::fs::read_to_string(dir.join("stubs/game.lua")).expect("stubs");
        assert!(
            stubs.contains(TEMPLATE_HOLDER),
            "the stubs lost their header"
        );

        let report = check_alone(&dir).expect("the new mod should resolve");
        assert!(
            report.is_ok(),
            "the template does not check clean: {:?}",
            report.failed
        );
        assert_eq!(
            report.loaded.len(),
            1,
            "the scratch set should hold the new mod alone: {:?}",
            report.loaded
        );
        assert!(
            report.loaded[0].starts_with("demo "),
            "the new mod is not the one loaded: {:?}",
            report.loaded
        );
        assert!(
            report.blocks.iter().any(|name| name == "demo:beacon"),
            "no beacon among {:?}",
            report.blocks
        );
    }

    #[test]
    fn a_template_checked_out_with_crlf_still_writes_lf() {
        // What a Windows checkout with `core.autocrlf` hands the compiler.
        let spec = NewMod::with_defaults("demo", None, None, None);
        let text = filled(
            b"-- SPDX-FileCopyrightText: Iridesium\r\n-- SPDX-License-Identifier: MIT\r\nid = \"{{MOD_ID}}\"\r\n",
            &spec,
        );
        assert_eq!(
            text,
            "-- SPDX-FileCopyrightText: Demo authors\n-- SPDX-License-Identifier: MIT\nid = \"demo\"\n"
        );
    }

    #[test]
    fn the_defaults_come_from_the_id_and_every_field_can_be_given() {
        let spec = NewMod::with_defaults("my_little_mod", None, None, None);
        assert_eq!(spec.name, "My Little Mod");
        assert_eq!(spec.license, "MIT");
        assert_eq!(spec.copyright, "My Little Mod authors");

        let spec = NewMod::with_defaults("x", Some("Ex"), Some("GPL-3.0-only"), Some("Somebody"));
        let into = scratch("given");
        let dir = create(&spec, &into).expect("scaffold");
        let readme = std::fs::read_to_string(dir.join("README.md")).expect("README");
        assert!(
            readme.starts_with(
                "<!-- SPDX-FileCopyrightText: Somebody -->\n\
                 <!-- SPDX-License-Identifier: GPL-3.0-only -->\n"
            ),
            "{readme}"
        );
        assert!(readme.contains("# Ex\n"), "{readme}");
        let manifest = std::fs::read_to_string(dir.join("mod.toml")).expect("mod.toml");
        assert!(
            manifest.contains("license = \"GPL-3.0-only\""),
            "{manifest}"
        );
    }

    #[test]
    fn a_bad_id_a_blank_field_and_an_existing_directory_are_refused() {
        let into = scratch("refused");
        assert!(matches!(
            create(&NewMod::with_defaults("Bad-Id", None, None, None), &into),
            Err(ScaffoldError::InvalidId(_))
        ));
        assert!(matches!(
            create(&NewMod::with_defaults("ok", Some(""), None, None), &into),
            Err(ScaffoldError::Blank("name"))
        ));
        assert!(matches!(
            create(
                &NewMod::with_defaults("ok", None, Some("MIT\" ; evil"), None),
                &into
            ),
            Err(ScaffoldError::Blank("licence"))
        ));
        assert!(!into.join("ok").exists(), "a refusal wrote something");

        create(&NewMod::with_defaults("ok", None, None, None), &into).expect("first");
        assert!(matches!(
            create(&NewMod::with_defaults("ok", None, None, None), &into),
            Err(ScaffoldError::Exists(_))
        ));
        assert!(
            into.join("ok/init.lua").is_file(),
            "the refusal touched the existing mod"
        );
    }
}
