// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! `mod.toml` parsing and mod directory discovery.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The file that makes a directory a mod.
pub const MANIFEST_FILE: &str = "mod.toml";

/// The entry script every mod must have.
pub const ENTRY_FILE: &str = "init.lua";

/// A choice a mod offers at WORLD CREATION, fixed for the life of the world.
///
/// **Declared in `mod.toml` and not in Lua, and that is the whole reason it
/// exists.** A `game.register_setting` arrives with a player, after the world
/// is made, so it cannot shape terrain — and the front screen has to be able
/// to show a world's options before any mod has run. A manifest is read
/// without a VM, so the launcher can draw these beside the seed box, and the
/// answer travels with the world exactly as the seed does: chosen once,
/// stored in the world file, handed to every VM that generates or ticks it.
///
/// ```toml
/// [[world_option]]
/// id = "biome"
/// name = "Biome"
/// description = "One biome everywhere, or the whole spindle."
/// options = ["spindle", "savanna", "taiga"]
/// default = 1
/// ```
///
/// No `options` is a toggle, with `default` 0 or 1. With them it is a choice,
/// and `default` is a ONE-BASED index into them — the same convention
/// `register_setting` uses, so a mod author never learns two.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldOption {
    /// Unqualified id, `snake_case`. Qualified against the mod as `mod:id`.
    pub id: String,
    /// What the screen calls it.
    pub name: String,
    /// One line under it.
    #[serde(default)]
    pub description: String,
    /// The choices, or none for a toggle.
    #[serde(default)]
    pub options: Vec<String>,
    /// The answer a world made without choosing gets: `0`/`1` for a toggle, a
    /// one-based index for a choice.
    #[serde(default = "WorldOption::default_default")]
    pub default: u32,
}

/// A mod's look, applied to the engine's OWN screens.
///
/// # Why the engine has no look of its own to defend
///
/// Charter rule 1: the mod API is the only API. The pause screen, the settings
/// pages and the start screen are the client's, drawn in plain egui, and a mod
/// has no hook into any of them — so a game built on this engine could restyle
/// its own dialogs and then hand the player back to a menu that looked like a
/// different program. Painting one game's art into the client instead would be
/// putting content in the engine, which is the same rule from the other side.
///
/// So a look is DATA a mod ships, and the engine applies it to its own
/// furniture. Nothing here is code and nothing here is drawn by the mod.
///
/// # Declared here and not in Lua
///
/// The same reason [`WorldOption`] is: **the start screen runs before any
/// server exists**, so no Lua has run and none can. A manifest is read without
/// a VM, which is what lets the launcher wear the theme of the mods it is
/// about to load.
///
/// ```toml
/// [theme]
/// font = "fonts/Cinzel.ttf"
/// sheet = "art/frame_iron.png"
/// button = "art/button_brass.png"
///
/// [theme.colours]
/// text = "#e8dcc0"
/// heading = "#f0d890"
/// background = "#1a1512"
/// button = "#2a2018"
/// accent = "#b08d57"
/// ```
///
/// Every field is optional and anything left out is the client's own look, so
/// a theme that names only a palette is a theme. **One theme applies at a
/// time — the last mod in load order that declares one** — because two mods
/// blending their idea of a frame is not a look, it is an accident.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Theme {
    /// A font file inside the mod's directory, drawn in place of the client's
    /// own on every engine screen.
    #[serde(default)]
    pub font: Option<String>,
    /// A nine-slice image for the frame around a sheet — the inventory, the
    /// pause screen, a settings page. Its border is a THIRD of the image, the
    /// rule `Style::nine_slice` already uses.
    #[serde(default)]
    pub sheet: Option<String>,
    /// A nine-slice image for the frame around a button.
    #[serde(default)]
    pub button: Option<String>,
    /// The palette. See [`ThemeColours`].
    #[serde(default)]
    pub colours: ThemeColours,
}

/// A theme's palette, as `"#rrggbb"` or `"#rrggbbaa"` strings.
///
/// Five, which is what it takes to restyle the engine's screens and no more.
/// A longer list would be the engine describing its own widget tree in a mod's
/// manifest, and every entry in it would be a promise about how the client is
/// built.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeColours {
    /// Ordinary text.
    #[serde(default)]
    pub text: Option<String>,
    /// Headings, and the title on a sheet's bar.
    #[serde(default)]
    pub heading: Option<String>,
    /// The fill behind a sheet, under any frame image.
    #[serde(default)]
    pub background: Option<String>,
    /// The fill behind a button.
    #[serde(default)]
    pub button: Option<String>,
    /// Selection, hover, and the active parts of a slider or a tick box.
    #[serde(default)]
    pub accent: Option<String>,
}

impl ThemeColours {
    /// Every colour in it, named, for a caller that has to check or convert
    /// them all.
    ///
    /// A method rather than five field reads at each call site: a sixth colour
    /// should be added in one place and reach every one of them, and the
    /// alternative is a validator that silently stops checking the new one.
    #[must_use]
    pub fn named(&self) -> [(&'static str, Option<&str>); 5] {
        [
            ("text", self.text.as_deref()),
            ("heading", self.heading.as_deref()),
            ("background", self.background.as_deref()),
            ("button", self.button.as_deref()),
            ("accent", self.accent.as_deref()),
        ]
    }
}

/// Parses `"#rrggbb"` or `"#rrggbbaa"` into straight RGBA bytes.
///
/// **Hex and not a table of numbers**, because a palette is copied out of the
/// program the art was drawn in, and every one of those shows hex.
///
/// Returns `None` for anything else, which the manifest turns into a named
/// error rather than a silently black screen.
#[must_use]
pub fn parse_colour(text: &str) -> Option<crate::ui::Colour> {
    let digits = text.strip_prefix('#')?;
    if digits.len() != 6 && digits.len() != 8 {
        return None;
    }
    if !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).ok();
    Some([
        byte(0)?,
        byte(2)?,
        byte(4)?,
        // No alpha written is opaque, which is what somebody pasting a colour
        // out of an art program means by it.
        if digits.len() == 8 { byte(6)? } else { 0xFF },
    ])
}

/// What a world chose for one option, as a mod reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorldOptionValue {
    /// A toggle's answer.
    Toggle(bool),
    /// A choice's answer: the option's TEXT, never its index, so a mod that
    /// inserts an option above it keeps comparing against the same string.
    Choice(String),
}

impl Theme {
    /// Checks a theme is one a client could actually wear.
    ///
    /// **Refused here rather than ignored later.** A colour that does not parse
    /// and a frame image nobody will ever fetch are both mistakes a mod author
    /// wants told about while they are editing the file — and the alternative,
    /// falling back silently, is a mod that looks like the engine ignoring it.
    ///
    /// # Errors
    ///
    /// [`ManifestError::BadTheme`] naming the field and what is wrong with it.
    pub fn validate(&self, id: &str) -> Result<(), ManifestError> {
        let bad = |reason: String| ManifestError::BadTheme {
            id: id.to_owned(),
            reason,
        };
        for (field, path) in [
            ("font", self.font.as_deref()),
            ("sheet", self.sheet.as_deref()),
            ("button", self.button.as_deref()),
        ] {
            let Some(path) = path else { continue };
            // **The same rule every other mod-supplied path obeys.** A theme's
            // files travel to clients through the content pipeline, and that
            // pipeline only carries distributable kinds — so a `.lua` or a
            // `.txt` here would be a file the server indexed and no client
            // could ever be sent.
            if !crate::content::is_distributable(Path::new(path)) {
                return Err(bad(format!(
                    "`{field} = \"{path}\"` is not a kind of file clients are sent"
                )));
            }
            // Nothing may climb out of the mod's own directory. The content
            // index applies this too; saying it here means the author is told
            // at the file they wrote rather than by a picture that never loads.
            if Path::new(path).is_absolute() || path.split(['/', '\\']).any(|part| part == "..") {
                return Err(bad(format!(
                    "`{field} = \"{path}\"` must be inside the mod's own directory"
                )));
            }
        }
        for (field, colour) in self.colours.named() {
            let Some(colour) = colour else { continue };
            if parse_colour(colour).is_none() {
                return Err(bad(format!(
                    "`colours.{field} = \"{colour}\"` is not `#rrggbb` or `#rrggbbaa`"
                )));
            }
        }
        Ok(())
    }

    /// The files this theme needs shipped to a client, in declaration order.
    #[must_use]
    pub fn files(&self) -> Vec<&str> {
        [
            self.font.as_deref(),
            self.sheet.as_deref(),
            self.button.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect()
    }
}

impl WorldOptionValue {
    /// The value as the world file and the wire carry it.
    ///
    /// Text either way — `"true"`/`"false"` for a toggle, the option's own text
    /// for a choice — so a world file is readable by a person and a renamed
    /// option is visibly a mismatch rather than an index pointing at the wrong
    /// thing.
    #[must_use]
    pub fn as_text(&self) -> String {
        match self {
            Self::Toggle(on) => if *on { "true" } else { "false" }.to_owned(),
            Self::Choice(text) => text.clone(),
        }
    }
}

impl WorldOption {
    fn default_default() -> u32 {
        1
    }

    /// Whether this is a checkbox rather than a dropdown.
    #[must_use]
    pub fn is_toggle(&self) -> bool {
        self.options.is_empty()
    }

    /// The id as it is stored and asked for: `mod:id`.
    #[must_use]
    pub fn qualified(&self, mod_id: &str) -> String {
        format!("{mod_id}:{}", self.id)
    }

    /// The declared default, as a value.
    #[must_use]
    pub fn default_value(&self) -> WorldOptionValue {
        if self.is_toggle() {
            WorldOptionValue::Toggle(self.default != 0)
        } else {
            let index = usize::try_from(self.default.saturating_sub(1)).unwrap_or(0);
            WorldOptionValue::Choice(
                self.options
                    .get(index)
                    .or_else(|| self.options.first())
                    .cloned()
                    .unwrap_or_default(),
            )
        }
    }

    /// What a world with `stored` as its answer chose, or the default where
    /// the stored text is missing or names nothing this declaration has.
    ///
    /// **A stored value that no longer matches is the default, not an error.**
    /// A mod that renames an option leaves every world made before the rename
    /// holding the old text; refusing to start those worlds would be worse
    /// than generating them with the mod's own fallback, and the mod can tell
    /// from `game.world_option` that it got the default.
    #[must_use]
    pub fn resolve(&self, stored: Option<&str>) -> WorldOptionValue {
        let Some(stored) = stored else {
            return self.default_value();
        };
        if self.is_toggle() {
            return match stored {
                "true" => WorldOptionValue::Toggle(true),
                "false" => WorldOptionValue::Toggle(false),
                _ => self.default_value(),
            };
        }
        if self.options.iter().any(|option| option == stored) {
            WorldOptionValue::Choice(stored.to_owned())
        } else {
            self.default_value()
        }
    }

    /// Checks one declaration.
    fn validate(&self, mod_id: &str) -> Result<(), ManifestError> {
        let bad = |reason: &str| ManifestError::BadWorldOption {
            id: mod_id.to_owned(),
            option: self.id.clone(),
            reason: reason.to_owned(),
        };
        if !is_valid_id(&self.id) {
            return Err(bad(
                "ids must be lowercase, start with a letter, and contain only letters, digits \
                 and underscores",
            ));
        }
        if self.name.trim().is_empty() {
            return Err(bad("it needs a `name` for the screen to show"));
        }
        if self.options.iter().any(|option| option.trim().is_empty()) {
            return Err(bad("an option's text cannot be empty"));
        }
        if self.is_toggle() {
            if self.default > 1 {
                return Err(bad("a toggle's `default` is 0 or 1"));
            }
        } else if self.default == 0 || self.default as usize > self.options.len() {
            return Err(bad(
                "`default` is a one-based index into `options`, so it must be at least 1 and \
                 at most their count",
            ));
        }
        Ok(())
    }
}

/// A mod's declared identity and dependencies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModManifest {
    /// Lowercase `snake_case` identifier. Also the mod's registration namespace.
    pub id: String,

    /// Human-readable name.
    pub name: String,

    /// Semantic version.
    pub version: String,

    /// Hard dependencies, as `"other_mod >=1.0, <2"`.
    #[serde(default)]
    pub depends: Vec<String>,

    /// Dependencies that only affect load order if present.
    ///
    /// The distinction is load order, not features: an optional dependency that
    /// is installed must load *first*, so the dependant can see what it
    /// registered. A mod that adds recipes for another mod's blocks needs
    /// exactly this.
    #[serde(default)]
    pub optional_depends: Vec<String>,

    /// Aliases this mod satisfies.
    ///
    /// Lets a fork or replacement stand in for the mod it replaces without
    /// every dependant being edited.
    #[serde(default)]
    pub provides: Vec<String>,

    /// One-line description.
    #[serde(default)]
    pub description: String,

    /// SPDX licence expression.
    #[serde(default)]
    pub license: String,

    /// Choices offered when a world is made. See [`WorldOption`].
    #[serde(default, rename = "world_option")]
    pub world_options: Vec<WorldOption>,

    /// How this mod wants the engine's own screens to look. See [`Theme`].
    #[serde(default)]
    pub theme: Option<Theme>,
}

/// A manifest could not be read or is not valid.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// The manifest file could not be read.
    #[error("could not read `{path}`")]
    Read {
        /// Path attempted.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// The manifest is not valid TOML, or has unknown fields.
    #[error("could not parse `{path}`")]
    Parse {
        /// Path attempted.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: toml::de::Error,
    },

    /// A mod id breaks the naming rules.
    #[error(
        "mod at `{path}` has id `{id}`: ids must be lowercase, start with a letter, and contain \
         only letters, digits and underscores"
    )]
    BadId {
        /// Path of the offending mod.
        path: PathBuf,
        /// The offending id.
        id: String,
    },

    /// A `[theme]` is malformed.
    #[error("mod `{id}` declares its theme wrongly: {reason}")]
    BadTheme {
        /// The mod.
        id: String,
        /// What is wrong with it.
        reason: String,
    },

    /// A `[[world_option]]` is malformed.
    #[error("mod `{id}` declares world option `{option}` wrongly: {reason}")]
    BadWorldOption {
        /// The mod.
        id: String,
        /// The option's own id, or what it had for one.
        option: String,
        /// What is wrong with it.
        reason: String,
    },

    /// A version is not valid semver.
    #[error("mod `{id}` has version `{version}`, which is not valid semver")]
    BadVersion {
        /// The mod.
        id: String,
        /// The offending version.
        version: String,
    },

    /// A dependency requirement could not be parsed.
    #[error("mod `{id}` declares dependency `{requirement}`, which is not a valid requirement")]
    BadRequirement {
        /// The mod declaring it.
        id: String,
        /// The offending requirement.
        requirement: String,
    },

    /// The mod has no `init.lua`.
    #[error("mod `{id}` at `{path}` has no {ENTRY_FILE}")]
    MissingEntry {
        /// The mod.
        id: String,
        /// Its directory.
        path: PathBuf,
    },

    /// A directory could not be scanned.
    #[error("could not scan mod directory `{path}`")]
    Scan {
        /// Path attempted.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

/// Whether an id obeys the naming rules.
///
/// Lowercase `snake_case`, starting with a letter. Strict because the id is also
/// the mod's registration namespace and appears in every string id it creates —
/// `MyMod:White` and `mymod:white` looking like different blocks would be a
/// permanent source of confusion.
#[must_use]
pub fn is_valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    id.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

impl ModManifest {
    /// Reads and validates a manifest from a mod directory.
    ///
    /// # Errors
    ///
    /// [`ManifestError`] if the file is missing, malformed, or declares
    /// something invalid.
    pub fn load(dir: &Path) -> Result<Self, ManifestError> {
        let path = dir.join(MANIFEST_FILE);
        let text = std::fs::read_to_string(&path).map_err(|source| ManifestError::Read {
            path: path.clone(),
            source,
        })?;
        let manifest: Self = toml::from_str(&text).map_err(|source| ManifestError::Parse {
            path: path.clone(),
            source,
        })?;
        manifest.validate(dir)?;
        Ok(manifest)
    }

    /// Checks everything that can be checked without seeing the other mods.
    ///
    /// # Errors
    ///
    /// [`ManifestError`] describing the first problem found.
    pub fn validate(&self, dir: &Path) -> Result<(), ManifestError> {
        if !is_valid_id(&self.id) {
            return Err(ManifestError::BadId {
                path: dir.to_path_buf(),
                id: self.id.clone(),
            });
        }

        semver::Version::parse(&self.version).map_err(|_| ManifestError::BadVersion {
            id: self.id.clone(),
            version: self.version.clone(),
        })?;

        for requirement in self.depends.iter().chain(&self.optional_depends) {
            parse_requirement(requirement).ok_or_else(|| ManifestError::BadRequirement {
                id: self.id.clone(),
                requirement: requirement.clone(),
            })?;
        }

        for alias in &self.provides {
            if !is_valid_id(alias) {
                return Err(ManifestError::BadId {
                    path: dir.to_path_buf(),
                    id: alias.clone(),
                });
            }
        }

        let mut seen = std::collections::BTreeSet::new();
        for option in &self.world_options {
            option.validate(&self.id)?;
            if !seen.insert(option.id.as_str()) {
                return Err(ManifestError::BadWorldOption {
                    id: self.id.clone(),
                    option: option.id.clone(),
                    reason: "declared twice".to_owned(),
                });
            }
        }

        if let Some(theme) = &self.theme {
            theme.validate(&self.id)?;
        }

        if !dir.join(ENTRY_FILE).is_file() {
            return Err(ManifestError::MissingEntry {
                id: self.id.clone(),
                path: dir.to_path_buf(),
            });
        }

        Ok(())
    }

    /// The parsed version. Valid after [`Self::validate`].
    ///
    /// # Errors
    ///
    /// If the version is not semver, which [`Self::validate`] would have caught.
    pub fn semver(&self) -> Result<semver::Version, ManifestError> {
        semver::Version::parse(&self.version).map_err(|_| ManifestError::BadVersion {
            id: self.id.clone(),
            version: self.version.clone(),
        })
    }
}

/// Splits `"other_mod >=1.0, <2"` into an id and a version requirement.
///
/// A bare `"other_mod"` means any version.
#[must_use]
pub fn parse_requirement(text: &str) -> Option<(String, semver::VersionReq)> {
    let text = text.trim();
    let (id, range) = match text.find(char::is_whitespace) {
        None => (text, "*"),
        Some(split) => (&text[..split], text[split..].trim()),
    };

    if !is_valid_id(id) {
        return None;
    }
    let requirement = semver::VersionReq::parse(range).ok()?;
    Some((id.to_owned(), requirement))
}

/// A mod found on disk: its manifest and where it lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredMod {
    /// The parsed manifest.
    pub manifest: ModManifest,
    /// The mod's directory.
    pub dir: PathBuf,
}

/// Scans a directory of mod directories.
///
/// **Results are sorted by id**, so the caller never sees filesystem order.
/// `read_dir` order varies between filesystems and even between runs on the
/// same one; letting it reach the resolver would make load order — and
/// therefore every numeric material id — machine-dependent.
///
/// # Errors
///
/// [`ManifestError`] if the directory cannot be read, or if any mod inside it
/// has an invalid manifest. A malformed mod is a hard failure rather than a
/// skip: silently ignoring a mod the operator installed is worse than refusing
/// to start.
pub fn scan_directory(root: &Path) -> Result<Vec<DiscoveredMod>, ManifestError> {
    let entries = std::fs::read_dir(root).map_err(|source| ManifestError::Scan {
        path: root.to_path_buf(),
        source,
    })?;

    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| ManifestError::Scan {
            path: root.to_path_buf(),
            source,
        })?;
        let dir = entry.path();
        if !dir.is_dir() || !dir.join(MANIFEST_FILE).is_file() {
            // A directory without a manifest is not a mod — documentation,
            // assets, a stray checkout. Not an error.
            continue;
        }
        found.push(DiscoveredMod {
            manifest: ModManifest::load(&dir)?,
            dir,
        });
    }

    found.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_rules_are_strict() {
        for good in ["core", "my_mod", "mod2", "a"] {
            assert!(is_valid_id(good), "{good} should be valid");
        }
        for bad in ["", "Core", "my-mod", "2mod", "my mod", "my.mod", "_leading"] {
            assert!(!is_valid_id(bad), "{bad} should be invalid");
        }
    }

    #[test]
    fn a_bare_requirement_means_any_version() {
        let (id, requirement) = parse_requirement("other_mod").expect("parse");
        assert_eq!(id, "other_mod");
        assert!(requirement.matches(&semver::Version::parse("0.1.0").expect("v")));
        assert!(requirement.matches(&semver::Version::parse("9.9.9").expect("v")));
    }

    #[test]
    fn a_ranged_requirement_parses_and_bounds() {
        let (id, requirement) = parse_requirement("other_mod >=1.0, <2").expect("parse");
        assert_eq!(id, "other_mod");
        assert!(!requirement.matches(&semver::Version::parse("0.9.0").expect("v")));
        assert!(requirement.matches(&semver::Version::parse("1.5.0").expect("v")));
        assert!(!requirement.matches(&semver::Version::parse("2.0.0").expect("v")));
    }

    #[test]
    fn a_theme_parses_out_of_the_manifest_it_shares_with_everything_else() {
        // **In `mod.toml` and not beside it**, for the reason a world option
        // is: the start screen has to read a theme before any mod has run, and
        // it already reads this file without a VM. A second file would be a
        // second discovery path and a second way to be malformed.
        // `r##` rather than `r#`: a hex colour ends in `"#`, which closes a
        // single-hash raw string in the middle of the fixture.
        let manifest: ModManifest = toml::from_str(
            r##"
            id = "iron"
            name = "Iron"
            version = "1.0.0"

            [theme]
            font = "fonts/Cinzel.ttf"
            sheet = "art/frame.png"
            button = "art/button.png"

            [theme.colours]
            text = "#e8dcc0"
            heading = "#f0d890"
            accent = "#b08d57ff"
            "##,
        )
        .expect("a manifest with a theme in it");
        let theme = manifest.theme.clone().expect("a theme");
        assert_eq!(theme.font.as_deref(), Some("fonts/Cinzel.ttf"));
        assert_eq!(
            theme.files(),
            ["fonts/Cinzel.ttf", "art/frame.png", "art/button.png"]
        );
        theme.validate("iron").expect("a valid theme");

        // A colour left out is the client's own, so the palette is a partial
        // statement rather than a thing a mod has to fill in.
        assert_eq!(theme.colours.background, None);
        assert_eq!(parse_colour("#e8dcc0"), Some([0xE8, 0xDC, 0xC0, 0xFF]));
        assert_eq!(parse_colour("#b08d5780"), Some([0xB0, 0x8D, 0x57, 0x80]));

        // And a manifest with no theme at all is every mod written so far.
        let plain: ModManifest =
            toml::from_str("id = \"plain\"\nname = \"Plain\"\nversion = \"1.0.0\"")
                .expect("a manifest");
        assert_eq!(plain.theme, None);
    }

    #[test]
    fn a_theme_that_could_not_be_worn_is_refused_at_the_manifest() {
        let of = |body: &str| {
            toml::from_str::<ModManifest>(&format!(
                "id = \"iron\"\nname = \"Iron\"\nversion = \"1.0.0\"\n{body}"
            ))
            .expect("parses")
            .theme
            .expect("a theme")
            .validate("iron")
        };

        // A colour nobody can read. Silently falling back would look exactly
        // like the engine ignoring the mod.
        let err = of("[theme.colours]\ntext = \"dark brown\"").expect_err("not hex");
        assert!(format!("{err}").contains("colours.text"), "{err}");
        assert!(
            of("[theme.colours]\ntext = \"#abc\"").is_err(),
            "three digits"
        );

        // A file kind the content pipeline will not carry, so no client could
        // ever be sent it however correct the rest of the theme is.
        let err = of("[theme]\nsheet = \"art/frame.bmp\"").expect_err("not distributable");
        assert!(format!("{err}").contains("sheet"), "{err}");

        // And nothing climbs out of the mod's own directory.
        assert!(of("[theme]\nfont = \"../../../etc/passwd\"").is_err());
        assert!(of("[theme]\nfont = \"/usr/share/fonts/x.ttf\"").is_err());

        // Non-vacuous: the same shape with the faults taken out passes.
        of("[theme]\nsheet = \"art/frame.png\"\n[theme.colours]\ntext = \"#e8dcc0\"")
            .expect("a theme with nothing wrong with it");
    }

    #[test]
    fn a_world_option_parses_and_is_checked() {
        let manifest: ModManifest = toml::from_str(
            r#"
id = "biomes"
name = "Biomes"
version = "1.0.0"

[[world_option]]
id = "biome"
name = "Biome"
options = ["spindle", "savanna"]
default = 2

[[world_option]]
id = "rivers"
name = "Rivers"
"#,
        )
        .expect("parses");
        assert_eq!(manifest.world_options.len(), 2);
        let biome = &manifest.world_options[0];
        assert!(!biome.is_toggle());
        assert_eq!(biome.qualified("biomes"), "biomes:biome");
        assert_eq!(
            biome.default_value(),
            WorldOptionValue::Choice("savanna".into())
        );
        assert_eq!(
            biome.resolve(Some("spindle")),
            WorldOptionValue::Choice("spindle".into())
        );
        // A value the option no longer has is the default, not an error.
        assert_eq!(
            biome.resolve(Some("tundra")),
            WorldOptionValue::Choice("savanna".into())
        );
        let rivers = &manifest.world_options[1];
        assert!(rivers.is_toggle());
        assert_eq!(rivers.default_value(), WorldOptionValue::Toggle(true));
        assert_eq!(
            rivers.resolve(Some("false")),
            WorldOptionValue::Toggle(false)
        );
        assert_eq!(
            rivers.resolve(Some("maybe")),
            WorldOptionValue::Toggle(true)
        );

        // And the checks, each the one a mod author would trip over.
        for (body, reason) in [
            ("id = \"Biome\"\nname = \"x\"", "capitals"),
            (
                "id = \"biome\"\nname = \"x\"\noptions = [\"a\"]\ndefault = 0",
                "a zero default",
            ),
            (
                "id = \"biome\"\nname = \"x\"\noptions = [\"a\"]\ndefault = 2",
                "a default past the end",
            ),
            (
                "id = \"biome\"\nname = \"x\"\ndefault = 2",
                "a toggle default of 2",
            ),
            ("id = \"biome\"\nname = \"\"", "an empty name"),
        ] {
            let text = format!(
                "id = \"m\"\nname = \"m\"\nversion = \"1.0.0\"\n[[world_option]]\n{body}\n"
            );
            let manifest: ModManifest = toml::from_str(&text).expect("parses");
            let dir = std::env::temp_dir();
            assert!(
                matches!(
                    manifest.validate(&dir),
                    Err(ManifestError::BadWorldOption { .. })
                ),
                "{reason} should be refused"
            );
        }
    }

    #[test]
    fn a_malformed_requirement_is_rejected() {
        assert!(parse_requirement("Bad_Id >=1.0").is_none());
        assert!(parse_requirement("ok_id !!!").is_none());
        assert!(parse_requirement("").is_none());
    }
}
