// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The Tiamat name registry.
//!
//! Charter rule 2 says the server is the game and no join may wait on the
//! network. This service is the one piece that is *not* on the join path: it
//! issues the short-lived **name certificate** that a server verifies offline
//! (`tiamat_core::identity::registry`), proves ownership of a name, and handles
//! the two flows that need mail — verifying a recovery address and recovering a
//! name from one.
//!
//! # What it is
//!
//! A headless HTTP service over SQLite, with no GUI, no game simulation, and no
//! dependencies a server binary does not already carry. The design of record is
//! `docs/accounts-design.md`; §7 is this crate. Nothing here is on the critical
//! path of play: the registry being down means a name cannot be claimed or
//! renewed that day, never that someone cannot play.
//!
//! # The shape
//!
//! - [`Config`] comes from the environment (see `Config::from_env`), because
//!   that is how Dokploy passes secrets.
//! - [`Service`] owns the database, the registry key, the mail transport, and
//!   the clock. It is the state every request reads.
//! - [`serve`] binds a loopback (or container) port and answers until stopped,
//!   through a bounded pool of workers under a two-second deadline per
//!   request. It is a library function with a thin binary on top so the bot
//!   tests (N6) can start the real service in-process with a mail sink in place
//!   of SMTP.
//! - [`healthcheck`] is the probe the container runs against itself, and
//!   [`work`] the proof of work an email request carries when its identity
//!   holds no name.
//!
//! # Hostile input
//!
//! Every request body arrives from the internet (charter rule 14). Bodies are
//! capped before they are read (32 KiB) and read under the request's deadline,
//! every signed record is parsed by `core::identity::registry` with its own
//! cap, and handlers are O(1) database work. A malformed request is a status
//! code, never a panic: this crate is `#![forbid(unsafe_code)]` and has no
//! `unwrap` outside tests.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod admin;
pub mod backup;
pub mod clock;
pub mod config;
pub mod db;
pub mod healthcheck;
pub mod hex;
pub mod keys;
pub mod mail;
pub mod service;
pub mod wire;
pub mod work;

mod api;
mod confusable;
mod deadline;
mod http;
mod limits;
mod mailquota;
mod names;
mod recover;
mod retention;
mod tokens;
mod workers;

#[cfg(test)]
mod stress;

pub use clock::{Clock, SystemClock};
pub use config::{Config, ConfigError};
pub use db::{Db, DbError, SCHEMA_VERSION};
pub use http::{Exit, MAX_BODY_BYTES, REQUEST_TIMEOUT, ServerHandle, serve, serve_with};
pub use keys::{GeneratedKey, GeneratedSuccessor, KeyError, RegistryKeys};
pub use mail::{Mail, MailError, Mailer, SinkMailer, SmtpMailer, Unavailable};
pub use mailquota::DEFAULT_MAILS_PER_HOUR;
pub use service::{Service, StartError};
