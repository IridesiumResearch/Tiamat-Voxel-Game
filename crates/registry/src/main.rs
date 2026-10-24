// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The registry binary: `serve`, and the operator's `admin` commands.
//!
//! The binary is deliberately thin. `serve` reads the environment and hands it
//! to the library; `admin` runs on the operator's own machine, with the keys on
//! disk and no network, because that is where a registry key is generated and
//! where a rotation is signed.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use tiamat_registry::hex;
use tiamat_registry::keys;
use tiamat_registry::mail::{SmtpMailer, Unavailable};
use tiamat_registry::{Config, Mailer, serve};

/// Command-line arguments.
#[derive(Debug, Parser)]
#[command(
    name = "registry",
    about = "The Tiamat name registry: names, certificates, and recovery",
    version
)]
struct Cli {
    /// What to do.
    #[command(subcommand)]
    command: Command,
}

/// The two things this binary does.
#[derive(Debug, Subcommand)]
enum Command {
    /// Serve the registry over HTTP.
    Serve,
    /// Operator commands, run on the machine that holds the keys.
    Admin {
        /// Which operator command.
        #[command(subcommand)]
        command: AdminCommand,
    },
}

/// Operator commands.
#[derive(Debug, Subcommand)]
enum AdminCommand {
    /// Make key material. Offline by construction; never talks to the service.
    Key {
        /// Which key operation.
        #[command(subcommand)]
        command: KeyCommand,
    },
}

/// Key operations.
#[derive(Debug, Subcommand)]
enum KeyCommand {
    /// The first key: a current key, its successor, and the commitment.
    ///
    /// Two files, both secrets. The current key goes into the host's
    /// `REGISTRY_KEY`; the successor's file is kept offline and becomes
    /// `REGISTRY_KEY` on the day of a rotation.
    New {
        /// Where to write the current key's seed.
        #[arg(long, value_name = "path")]
        current_out: PathBuf,
        /// Where to write the successor's seed. Keep this offline.
        #[arg(long, value_name = "path")]
        successor_out: PathBuf,
    },

    /// A successor for a key you already hold: what a rotation needs.
    ///
    /// Prints the commitment for the current key to carry and writes the
    /// successor's seed for the day it takes over.
    Next {
        /// Where to write the successor's seed. Keep this offline.
        #[arg(long, value_name = "path")]
        successor_out: PathBuf,
    },
}

fn main() -> ExitCode {
    // Presentation-layer only, as everywhere in this project. `try_init`
    // rather than `init` so a test harness that set a subscriber first is not
    // a panic.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .try_init();

    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("registry: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Runs one command.
fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    match cli.command {
        Command::Serve => serve_command(),
        Command::Admin { command } => admin_command(command),
    }
}

/// Serves until interrupted.
fn serve_command() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_env()?;
    tracing::debug!(?config, "starting the registry");

    // A configured SMTP URL is a startup dependency: a registry that boots
    // without mail and only says so when a player asks for a link has hidden
    // the error behind a support conversation.
    let mailer: Arc<dyn Mailer> = match (&config.mail_from, &config.smtp_url) {
        (Some(from), Some(url)) => {
            tracing::info!(from, "mail is configured");
            Arc::new(SmtpMailer::new(url, from)?)
        }
        _ => {
            tracing::warn!("no mail is configured; recovery and email links cannot be sent");
            Arc::new(Unavailable)
        }
    };
    let handle = serve(config, mailer)?;
    tracing::info!(addr = %handle.local_addr(), "the registry is listening");

    wait_for_interrupt()?;
    handle.stop();
    tracing::info!("the registry stopped");
    Ok(())
}

/// Blocks until Ctrl-C.
fn wait_for_interrupt() -> Result<(), Box<dyn std::error::Error>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    ctrlc::set_handler(move || {
        // The receiver may already be gone during shutdown; that is not an
        // error worth reporting from a signal handler.
        let _ = sender.send(());
    })?;
    receiver.recv()?;
    Ok(())
}

/// Runs an operator command.
fn admin_command(command: AdminCommand) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        AdminCommand::Key { command } => key_command(command),
    }
}

/// Makes key material.
fn key_command(command: KeyCommand) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        KeyCommand::New {
            current_out,
            successor_out,
        } => {
            let generated = keys::generate()?;
            write_secret(&current_out, &generated.seed)?;
            write_secret(&successor_out, &generated.successor_seed)?;
            println!(
                "current public key:    {}",
                hex::encode(generated.public.as_bytes())
            );
            println!(
                "successor public key:  {}",
                hex::encode(generated.successor_public.as_bytes())
            );
            println!(
                "REGISTRY_NEXT_KEY_HASH={}",
                hex::encode(&generated.next_commitment)
            );
            println!();
            println!(
                "Set REGISTRY_KEY from {}. Keep {} offline: it becomes",
                current_out.display(),
                successor_out.display()
            );
            println!("REGISTRY_KEY on the day of a rotation, and nothing before that.");
        }
        KeyCommand::Next { successor_out } => {
            let successor = keys::generate_successor()?;
            write_secret(&successor_out, &successor.seed)?;
            println!(
                "successor public key:  {}",
                hex::encode(successor.public.as_bytes())
            );
            println!(
                "REGISTRY_NEXT_KEY_HASH={}",
                hex::encode(&successor.commitment)
            );
            println!();
            println!(
                "The current key must carry that commitment before the rotation. Keep {} offline.",
                successor_out.display()
            );
        }
    }
    Ok(())
}

/// Writes a seed to a file only its owner can read, as lowercase hex.
///
/// A registry seed in a world-readable file is a registry anyone can sign with.
/// The file is created with default permissions first and restricted
/// immediately; on Unix that is `0600`. On Windows the file inherits the
/// profile's ACL, as the identity key file does, and the operator's profile is
/// the boundary there.
fn write_secret(path: &Path, seed: &[u8; 32]) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::File::create(path)?;
    writeln!(file, "{}", hex::encode(seed))?;
    restrict(path)?;
    Ok(())
}

/// Makes a key file readable only by its owner.
///
/// Infallible on Windows (the profile's ACL is the boundary) and fallible on
/// Unix; one signature for both callers, as the identity key file's helper does.
#[allow(
    clippy::unnecessary_wraps,
    reason = "infallible on Windows, fallible on Unix; one signature for both callers"
)]
fn restrict(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}
