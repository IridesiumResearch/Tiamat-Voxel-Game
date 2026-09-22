// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The launcher: applies a staged update, then starts the game.
//!
//! [`docs/distribution.md`](../../../docs/distribution.md) §1 and §5. This is
//! the binary a player's shortcut points at, and **the one component an update
//! cannot replace** — which is why it is small, has no network and no window,
//! and does as little as a thing can do that still does the job.
//!
//! # Why the launcher applies and the client downloads
//!
//! Nothing may overwrite a running binary. The client has a window and can
//! show a 150 MB download happening; by the time this runs, the bytes are on
//! disk, verified, and the work is three renames. So the client stages and
//! exits, and this applies before anything is open.
//!
//! # What it checks, again
//!
//! The client checked the signature before it downloaded and the hash after.
//! This checks both again, because between the two runs the staged files sat
//! on a disk that other programs can write to, and an update applied from a
//! directory nobody re-checked is an update anybody could have edited.

use std::path::{Path, PathBuf};

use updater::install::{Install, InstallError};

/// The public key this build trusts to sign a release.
///
/// Stamped at compile time by the release workflow. **A build without one
/// applies no updates at all** and says so, which is what a working copy
/// should do: a developer's build has no business installing anything.
const RELEASE_KEY: Option<&str> = option_env!("TIAMOT_RELEASE_KEY");

fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    let mut launch = true;
    let mut passthrough: Vec<String> = Vec::new();
    let mut command = Command::Run;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--status" => command = Command::Status,
            "--rollback" => command = Command::Rollback,
            "--no-launch" => launch = false,
            "--help" | "-h" => {
                println!("{HELP}");
                return std::process::ExitCode::SUCCESS;
            }
            // Everything after `--` belongs to the game.
            "--" => passthrough.extend(args.by_ref()),
            other => passthrough.push(other.to_owned()),
        }
    }

    match run(command, launch, &passthrough) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("tiamot: {err}");
            eprintln!();
            eprintln!("The game itself is in `current`, and can be started directly.");
            eprintln!("If it will not, unpack a fresh download beside this one.");
            std::process::ExitCode::FAILURE
        }
    }
}

const HELP: &str = "\
tiamot — starts the game, applying any update that is waiting.

    --status     say what is installed and what is staged, and do nothing
    --rollback   put the previous version back
    --no-launch  apply an update but do not start the game
    --           everything after this is passed to the game

Updates are downloaded by the game itself and applied here, before anything
is running. See docs/distribution.md.";

enum Command {
    Run,
    Status,
    Rollback,
}

fn run(
    command: Command,
    launch: bool,
    passthrough: &[String],
) -> Result<std::process::ExitCode, InstallError> {
    let root = install_root()?;
    let install = Install::open(&root)?;

    match command {
        Command::Status => {
            install.report();
            return Ok(std::process::ExitCode::SUCCESS);
        }
        Command::Rollback => {
            install.rollback()?;
            println!("put the previous version back");
            return Ok(std::process::ExitCode::SUCCESS);
        }
        Command::Run => {}
    }

    // **A staged update is applied before anything starts**, and a failure to
    // apply one is not a failure to play: the version that is already here
    // still works, so this says what went wrong and carries on.
    match install.apply_staged(RELEASE_KEY) {
        Ok(Some(version)) => println!("updated to {version}"),
        Ok(None) => {}
        Err(err) => {
            eprintln!("tiamot: the waiting update was not applied: {err}");
            eprintln!("tiamot: starting the version already installed.");
            install.discard_staged();
        }
    }

    if !launch {
        return Ok(std::process::ExitCode::SUCCESS);
    }
    install.launch(passthrough)
}

/// Where this launcher is, which is where the install is.
///
/// The executable's own directory rather than the working directory: a
/// shortcut, a Finder double-click and a terminal all start a program with a
/// different idea of where it is, and only one of them is right.
fn install_root() -> Result<PathBuf, InstallError> {
    let exe = std::env::current_exe().map_err(|source| InstallError::Io {
        what: "find this program".to_owned(),
        source,
    })?;
    let dir = exe.parent().unwrap_or(Path::new(".")).to_path_buf();
    Ok(dir)
}
