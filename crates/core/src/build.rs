// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Which build this is: version, channel, and the commit it was made from.
//!
//! **Stamped at compile time, never read from a file.** A build that could be
//! told what version it is could be told the wrong one, and the whole update
//! mechanism turns on a client knowing honestly what it already has — see
//! [`docs/distribution.md`](../../../docs/distribution.md) §4.
//!
//! In a working copy the commit is absent and the channel is `dev`, which is
//! exactly what a developer's build should say it is.

/// The version in `Cargo.toml`, shared by every crate in the workspace.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The commit this was built from, when the release workflow said so.
///
/// `None` in a working copy, which is how "a build somebody made themselves"
/// is told from "a build that came from a release" without asking anybody.
pub const COMMIT: Option<&str> = option_env!("TIAMAT_COMMIT");

/// Which update channel this build follows.
///
/// **Compiled in, and there is no switching at runtime.** A build that could
/// move itself between channels could be talked into moving; a channel is a
/// separate manifest URL and therefore a separate build.
pub const CHANNEL: &str = match option_env!("TIAMAT_CHANNEL") {
    Some(channel) => channel,
    None => "dev",
};

/// Where this build looks for its update manifest, if it was given one.
///
/// `None` for a working copy, which is what stops a developer's build from
/// asking a website about itself on every run.
pub const MANIFEST_URL: Option<&str> = option_env!("TIAMAT_MANIFEST_URL");

/// A one-line description of this build, for a log line or a corner of a
/// screen: `0.2.0 (test, 8929ca1)`, or `0.1.0 (dev)`.
#[must_use]
pub fn describe() -> String {
    match COMMIT {
        Some(commit) => {
            let short = commit.get(..7).unwrap_or(commit);
            format!("{VERSION} ({CHANNEL}, {short})")
        }
        None => format!("{VERSION} ({CHANNEL})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_working_copy_says_it_is_a_working_copy() {
        // The test suite runs without the release workflow's variables, so
        // this is the developer's build describing itself — and it must not
        // claim a channel it does not follow.
        assert_eq!(CHANNEL, "dev", "a working copy is on no release channel");
        assert!(COMMIT.is_none(), "a working copy names no commit");
        assert!(
            MANIFEST_URL.is_none(),
            "a working copy asks nobody for updates"
        );
        assert!(
            describe().contains(VERSION) && describe().contains("dev"),
            "{}",
            describe()
        );
    }
}
