// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The reference mods, and only them, for a server a test or benchmark starts.
//!
//! `game/` holds the reference mods — the tracked `core_*` directories — but it
//! is also where a developer keeps whatever they are building or playing: a mod
//! in progress, a symlink to where they really keep it, a fixture copied in
//! from `docs/fixtures/` (`.gitignore` says so). A server pointed at `game/`
//! loads all of it, so a test that did so was testing somebody's world mod as
//! well as the engine. One that moves every joining player to its own spawn
//! failed tests across the suite at once — a dig "too far away", terrain that
//! was not the seed's — and none of it was a bug in the engine.
//!
//! [`enabled_mods_for`] is what to pass as `enabled_mods` beside a `mods_path`:
//! the set CI has when that path is `game/`, whatever else is in it, and
//! everything when it is a directory a test wrote for itself.

use std::path::{Path, PathBuf};

use tiamat_core::modload::{ManifestError, ModManifest};

/// The prefix every reference mod's directory has, and nothing else in `game/`.
const REFERENCE_PREFIX: &str = "core_";

/// `game/`, at the root of the repository this crate was built from.
#[must_use]
pub fn game_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../game")
}

/// The `enabled_mods` for a server loading from `mods`.
///
/// `Some(reference mods)` when `mods` is `game/`, and `None` — every mod in
/// it — when it is anywhere else, which for a test is a directory holding
/// exactly the mods it wrote.
///
/// # Errors
///
/// [`ManifestError`] if `mods` is `game/` and a reference mod's manifest
/// cannot be read.
pub fn enabled_mods_for(mods: &Path) -> Result<Option<Vec<String>>, ManifestError> {
    let game = game_dir();
    let is_game = match (mods.canonicalize(), game.canonicalize()) {
        (Ok(mods), Ok(game)) => mods == game,
        _ => false,
    };
    if is_game {
        reference_mod_ids(&game).map(Some)
    } else {
        Ok(None)
    }
}

/// The ids of the reference mods in `game`, sorted.
///
/// Ids rather than directory names, because the two differ — `core_blocks`
/// registers as `core`.
///
/// # Errors
///
/// [`ManifestError`] if `game` cannot be listed or a reference mod's manifest
/// cannot be read.
pub fn reference_mod_ids(game: &Path) -> Result<Vec<String>, ManifestError> {
    let read_error = |source| ManifestError::Read {
        path: game.to_path_buf(),
        source,
    };
    let mut ids = Vec::new();
    for entry in std::fs::read_dir(game).map_err(read_error)? {
        let entry = entry.map_err(read_error)?;
        let is_reference = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(REFERENCE_PREFIX));
        if is_reference && entry.path().is_dir() {
            ids.push(ModManifest::load(&entry.path())?.id);
        }
    }
    ids.sort();
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_is_narrowed_to_the_core_mods_by_id() {
        let ids = enabled_mods_for(&game_dir())
            .expect("the reference mods")
            .expect("game/ is narrowed");
        assert!(
            ids.contains(&"core".to_owned()),
            "core_blocks is `core`: {ids:?}"
        );
        assert!(ids.contains(&"core_tools".to_owned()), "{ids:?}");
        assert!(
            ids.iter()
                .all(|id| id == "core" || id.starts_with(REFERENCE_PREFIX)),
            "only reference mods: {ids:?}"
        );
    }

    #[test]
    fn a_directory_a_test_wrote_is_loaded_whole() {
        let dir = std::env::temp_dir().join("tiamat-bot-fixture-whole");
        std::fs::create_dir_all(&dir).expect("scratch dir");
        assert_eq!(enabled_mods_for(&dir).expect("no manifests read"), None);
    }

    #[test]
    fn a_mod_beside_the_reference_mods_is_left_out() {
        let dir = std::env::temp_dir().join("tiamat-bot-fixture-beside");
        let _ = std::fs::remove_dir_all(&dir);
        for id in ["core_thing", "my_world"] {
            std::fs::create_dir_all(dir.join(id)).expect("scratch mod");
            std::fs::write(
                dir.join(id).join("mod.toml"),
                format!("id = \"{id}\"\nname = \"{id}\"\nversion = \"0.1.0\"\n"),
            )
            .expect("manifest");
            std::fs::write(dir.join(id).join("init.lua"), "").expect("entry");
        }
        assert_eq!(
            reference_mod_ids(&dir).expect("scan"),
            vec!["core_thing".to_owned()]
        );
    }
}
