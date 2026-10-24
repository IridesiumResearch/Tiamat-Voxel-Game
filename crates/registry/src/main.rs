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
use tiamat_registry::{Config, Mailer, admin, serve};

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

    /// Print what the deployment holds, in one screen.
    Stats {
        /// How many days of activity to break down.
        #[arg(long, value_name = "days", default_value_t = 7)]
        days: i64,
    },

    /// Group this window's claims by network, biggest first.
    Claims {
        /// How far back to look: `48h`, `30m`, `7d`.
        #[arg(long, value_name = "window", default_value = "48h")]
        since: String,
    },

    /// Revoke every name claimed from a network in a window.
    ///
    /// The burst remedy: one line undoes a squatter's morning, and the reason
    /// goes to the audit table and the revocation list.
    Revoke {
        /// The network, as it is written in the claim log: `203.0.113.0/24`.
        #[arg(long, value_name = "subnet")]
        subnet: String,
        /// How far back to look: `48h`, `30d`.
        #[arg(long, value_name = "window", default_value = "48h")]
        since: String,
        /// Why, in the operator's words. Required.
        #[arg(long, value_name = "text")]
        reason: String,
    },

    /// Reserve a name so no claim may take it.
    Reserve {
        /// The canonical name.
        name: String,
    },

    /// Release a reservation.
    Unreserve {
        /// The canonical name.
        name: String,
    },

    /// Change the display casing of a held name; the canonical form cannot move.
    Rename {
        /// The holder's UUID, 64 hex characters.
        #[arg(long, value_name = "hex")]
        uuid: String,
        /// The new display casing.
        #[arg(long, value_name = "name")]
        display: String,
    },

    /// Publish a rotation to a successor key.
    ///
    /// Run with the **current** key still in the environment: the record is
    /// signed by the key being retired. Afterwards set `REGISTRY_KEY` from the
    /// successor's file and `REGISTRY_NEXT_KEY_HASH` to the new commitment.
    Rotate {
        /// The successor's seed file, as written by `admin key next`.
        #[arg(long, value_name = "path")]
        successor: PathBuf,
        /// The successor's own commitment, from a fresh `admin key next`.
        #[arg(long, value_name = "hex")]
        next: String,
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
        AdminCommand::Stats { days } => with_admin(|admin| {
            let stats = admin.stats()?;
            println!("names held:          {}", stats.held);
            println!("names released:      {}", stats.released);
            println!("recovery addresses:  {}", stats.emails);
            println!("revoked serials:     {}", stats.revocations);
            println!("recovery notices:    {}", stats.pending_recoveries);
            println!("claims in the last day: {}", stats.claims_today);
            let activity = admin.daily_activity(days)?;
            if !activity.is_empty() {
                println!();
                println!("claims / recovery notices by day:");
                for day in activity {
                    println!(
                        "  {}  {:>4} claims  {:>3} recoveries",
                        day.date, day.claims, day.recoveries
                    );
                }
            }
            Ok(())
        }),
        AdminCommand::Claims { since } => with_admin(|admin| {
            let since = admin::parse_window(&since, admin.now()).map_err(failure)?;
            let groups = admin.claims_by_subnet(since)?;
            if groups.is_empty() {
                println!("no claims in that window");
            }
            for group in groups {
                println!("{:>6}  {}", group.claims, group.subnet);
            }
            Ok(())
        }),
        AdminCommand::Revoke {
            subnet,
            since,
            reason,
        } => with_admin(|admin| {
            let since = admin::parse_window(&since, admin.now()).map_err(failure)?;
            let revoked = admin.revoke_subnet(&subnet, since, &reason)?;
            println!(
                "released {} name(s) and revoked {} certificate(s) from {subnet}",
                revoked.names, revoked.serials
            );
            println!("the reason is in the audit table, and the serials on the revocation list");
            Ok(())
        }),
        AdminCommand::Reserve { name } => with_admin(|admin| {
            admin.reserve(&name)?;
            println!("reserved `{name}`");
            Ok(())
        }),
        AdminCommand::Unreserve { name } => with_admin(|admin| {
            if admin.unreserve(&name)? {
                println!("released the reservation on `{name}`");
            } else {
                println!("`{name}` was not reserved");
            }
            Ok(())
        }),
        AdminCommand::Rename { uuid, display } => with_admin(|admin| {
            let name = admin.rename(&uuid, &display)?;
            println!("{} now shows as `{}`", name.canonical(), name.display());
            Ok(())
        }),
        AdminCommand::Rotate { successor, next } => with_admin(|admin| {
            let seed = read_secret(&successor)?;
            let next = hex::decode::<32>(&next)
                .ok_or("--next must be 64 hex characters, the commitment from `admin key next`")?;
            let record = admin.rotate(seed, next)?;
            println!(
                "rotation published, signed by {}",
                hex::encode(&record.retired)
            );
            println!("successor public key: {}", hex::encode(&record.successor));
            println!();
            println!("Now set, and restart the service:");
            println!("  REGISTRY_KEY  = the seed in {}", successor.display());
            println!("  REGISTRY_NEXT_KEY_HASH={}", hex::encode(&next));
            Ok(())
        }),
    }
}

/// Runs a command that needs the deployment's database.
fn with_admin(
    command: impl FnOnce(&admin::Admin) -> Result<(), Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_env()?;
    let admin = admin::Admin::open(&config)?;
    command(&admin)
}

/// A one-line failure for the operator.
fn failure(message: String) -> Box<dyn std::error::Error> {
    message.into()
}

/// Reads a seed from the hex file `admin key new` or `key next` wrote.
fn read_secret(path: &Path) -> Result<[u8; 32], Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(path)?;
    hex::decode::<32>(text.trim())
        .ok_or_else(|| format!("{} does not hold a 64-character hex seed", path.display()).into())
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
