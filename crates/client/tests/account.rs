// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The account a client carries to a server (accounts N4).
//!
//! The store's own tests live beside it in `client::account`; this drives the
//! real thing a test can only check at the seam: a certificate read from the
//! file, presented on the challenge answer, and honoured by a server that pins
//! the registry key that issued it.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use client::account::{ACCOUNT_FILE, Account};
use client::cache::ContentCache;
use client::net::{Connection, Event, Pinning};
use tiamat_core::identity::registry::{Name, NameCertificate, RegistryTrust};
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::interest::ViewDistance;
use tiamat_server::{ServerHandle, Settings};

/// Long enough for a cold start and a loopback join.
const PATIENCE: Duration = Duration::from_secs(10);

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("tiamat-client-accounts")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(0))
}

/// A server that honours certificates issued by `trust`.
fn start(name: &str, trust: RegistryTrust) -> ServerHandle {
    ServerHandle::start(&Settings {
        world_options: Vec::new(),
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: scratch(&format!("{name}-world")),
        identity_path: None,
        max_players: 2,
        allow_custom_skins: false,
        sticky_names: false,
        name_policy: tiamat_core::session::NamePolicy::Honour,
        registry_trust: Some(trust),
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: None,
        enabled_mods: None,
        seed: Some(11),
        rcon: None,
        // One material, because the client joins only after the material table
        // has arrived; an empty table is a table it waits on for ever.
        materials: vec!["test:stone".to_owned()],
    })
    .expect("the server must start")
}

/// A certificate the test registry issued to `identity`.
fn certificate(
    registry: &ed25519_dalek::SigningKey,
    identity: &Identity,
    canonical: &str,
    display: &str,
) -> NameCertificate {
    let name = Name::from_parts(canonical, display).expect("a valid name");
    NameCertificate::issue(registry, &name, identity.uuid_as_root(), unix_now(), 1)
}

/// Polls until the client is in the world, or fails with what it saw.
fn join(connection: &mut Connection) -> [u8; 32] {
    let deadline = Instant::now() + PATIENCE;
    while Instant::now() < deadline {
        match connection.poll() {
            Some(Event::Joined { uuid, .. }) => return uuid,
            Some(Event::Disconnected { reason }) => {
                panic!("the server refused the join: {reason:?}")
            }
            Some(_) => {}
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    panic!("the client never reached the world within {PATIENCE:?}");
}

fn open(server: &ServerHandle, data: &Path, identity: Identity, account: &Account) -> Connection {
    Connection::open_with_account(
        server.local_addr(),
        identity,
        "Typed by hand".to_owned(),
        ContentCache::open(&data.join("content")).expect("cache"),
        Pinning::OwnServer,
        account.present().expect("a stored account presents"),
    )
    .expect("the transport should connect")
}

#[test]
fn a_stored_certificate_joins_as_the_name_the_registry_issued() {
    let registry = ed25519_dalek::SigningKey::from_bytes(&[0x7A; 32]);
    let trust = RegistryTrust::new(registry.verifying_key(), None);
    let server = start("certified", trust);
    let data = scratch("certified-data");

    let identity = Identity::generate().expect("an identity");
    let uuid = identity.uuid_as_root();
    let mut account = Account::default();
    account
        .set_certificate(
            &identity,
            certificate(&registry, &identity, "alice", "Alice"),
        )
        .expect("its own certificate");
    account
        .save(&data.join(ACCOUNT_FILE))
        .expect("the account should save");
    let loaded =
        Account::load(&data.join(ACCOUNT_FILE), &identity).expect("the account should load");

    let mut connection = open(&server, &data, identity, &loaded);
    let joined_as = join(&mut connection);

    assert_eq!(
        joined_as,
        *uuid.as_bytes(),
        "the server resolved the identity that signed"
    );
    let players = server.shared().online_players();
    assert!(
        players
            .iter()
            .any(|(who, name)| *who == uuid && name == "Alice"),
        "the certified name should be what the server shows, got {players:?}"
    );

    connection.shutdown();
    server.stop();
}

#[test]
fn an_empty_account_joins_under_the_name_the_client_asked_for() {
    // The ordinary path, and the one that must not change: no certificate, no
    // registry answer, the name the player typed.
    let registry = ed25519_dalek::SigningKey::from_bytes(&[0x7B; 32]);
    let trust = RegistryTrust::new(registry.verifying_key(), None);
    let server = start("uncertified", trust);
    let data = scratch("uncertified-data");
    let identity = Identity::generate().expect("an identity");
    let uuid = identity.uuid_as_root();

    let mut connection = open(&server, &data, identity, &Account::default());
    let joined_as = join(&mut connection);
    assert_eq!(joined_as, *uuid.as_bytes());

    let players = server.shared().online_players();
    assert!(
        players
            .iter()
            .any(|(who, name)| *who == uuid && name == "Typed by hand"),
        "without a certificate the asked-for name must stand, got {players:?}"
    );

    connection.shutdown();
    server.stop();
}
