// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Name certificates on a real server (accounts N3, design §5).
//!
//! The unit tests in `core::session` decide what a certificate means; these
//! drive a real client against a real loopback server, because the parts that
//! can only be wrong at the seam are the ones this file is for: the policy
//! read from `server.toml`'s shape, the displacement notice reaching the player
//! it names, and the mod API's `event.certified` / `game.player_certified`.

use std::path::PathBuf;
use std::time::Duration;

use bot::Bot;
use tiamat_core::identity::registry::{Name, NameCertificate, RegistryTrust};
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::proto::{DisconnectReason, ServerMessage};
use tiamat_core::session::NamePolicy;
use tiamat_server::{ServerHandle, Settings};

/// The registry's signing key for one test, and what a server pins for it.
///
/// A fresh key per test rather than a shared constant: two tests running in
/// parallel must not be able to accept each other's certificates.
fn registry() -> (ed25519_dalek::SigningKey, RegistryTrust) {
    let key = ed25519_dalek::SigningKey::from_bytes(&[0x5A; 32]);
    let trust = RegistryTrust::new(key.verifying_key(), None);
    (key, trust)
}

/// A certificate for `identity`, issued just now and valid for the canonical
/// lifetime the registry gives one.
fn certificate(
    registry: &ed25519_dalek::SigningKey,
    identity: &Identity,
    canonical: &str,
    display: &str,
) -> Vec<u8> {
    let name = Name::from_parts(canonical, display).expect("a valid name");
    let certificate =
        NameCertificate::issue(registry, &name, identity.uuid_as_root(), unix_now(), 1);
    certificate.to_bytes().expect("a certificate encodes")
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(0))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("tiamat-certified-names")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(future)
}

/// A server with the given policy and a registry to check against.
fn start(
    name: &str,
    policy: NamePolicy,
    trust: Option<RegistryTrust>,
    mods_path: Option<PathBuf>,
) -> ServerHandle {
    ServerHandle::start(&Settings {
        world_options: Vec::new(),
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: scratch(&format!("{name}-world")),
        identity_path: None,
        max_players: 8,
        allow_custom_skins: false,
        sticky_names: false,
        name_policy: policy,
        registry_trust: trust,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: tiamat_core::interest::ViewDistance::MINIMUM,
        mods_path,
        enabled_mods: None,
        seed: Some(3),
        rcon: None,
        materials: Vec::new(),
    })
    .expect("start")
}

async fn connect(server: &ServerHandle, identity: Identity) -> Bot {
    Bot::connect(server.local_addr(), identity, server.cert_fingerprint())
        .await
        .expect("connect")
}

/// Waits for a line of chat from the server itself (from nobody).
async fn server_says(bot: &mut Bot, needle: &str) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!left.is_zero(), "the server never said `{needle}`");
        let message = tokio::time::timeout(left, bot.recv())
            .await
            .expect("the server went quiet")
            .expect("a message, not an error");
        if let ServerMessage::Chat { from: None, text } = message
            && text.contains(needle)
        {
            return text;
        }
    }
}

#[test]
fn a_certified_name_wins_and_the_displaced_holder_is_told_why() {
    let (registry, trust) = registry();
    let server = start("displaced", NamePolicy::Honour, Some(trust), None);
    let alice_identity = Identity::generate().expect("identity");
    let alice_uuid = alice_identity.uuid_as_root();

    block_on(async {
        // First-come on this server: Alice is uncertified and wears it.
        let mut alice = connect(&server, alice_identity).await;
        alice.join("Alice").await.expect("join");

        // The registry's Alice arrives. Her certificate's casing is what the
        // server shows, whatever she typed into her client.
        let bob_identity = Identity::generate().expect("identity");
        let bob_uuid = bob_identity.uuid_as_root();
        let certificate = certificate(&registry, &bob_identity, "alice", "Alice");
        let mut bob = connect(&server, bob_identity).await;
        bob.join_with_certificate("Bob", &certificate)
            .await
            .expect("a certified join");

        // The displaced player keeps playing and is told, in the server's own
        // voice, why the name moved.
        let told = server_says(&mut alice, "registered to another identity").await;
        assert!(
            told.contains("`Alice 2`"),
            "the notice should name the variant they now wear: {told}"
        );

        // The binding is what the registry said: Alice is Bob's, and Alice is
        // `Alice 2` here.
        let identities = server.shared().identities.lock().await;
        assert_eq!(identities.name_of(&bob_uuid), Some("Alice"));
        assert_eq!(identities.name_of(&alice_uuid), Some("Alice 2"));
        drop(identities);

        // The roster moved with the binding — `status`, `kick` and every mod
        // that asks who is here read it — not only the database row.
        assert!(
            server
                .shared()
                .online_players()
                .iter()
                .any(|(who, name)| *who == alice_uuid && name == "Alice 2"),
            "the displaced player's roster entry must show the variant"
        );

        // **An uncertified player cannot take a certified name**, even while
        // its holder is here to hold it: the binding is the registry's, so a
        // later claimant gets the next free variant and is told why.
        let mut carol = connect(&server, Identity::generate().expect("identity")).await;
        carol.join("Alice").await.expect("join");
        let told = server_says(&mut carol, "already somebody else's name").await;
        assert!(
            told.contains("`Alice 3`"),
            "an uncertified claimant must be moved off the certified name: {told}"
        );
        carol.disconnect().await;

        bob.disconnect().await;
        alice.disconnect().await;
    });

    server.stop();
}

#[test]
fn require_refuses_a_join_with_no_certificate() {
    let (registry, trust) = registry();
    let server = start("require", NamePolicy::Require, Some(trust), None);

    block_on(async {
        let mut stranger = connect(&server, Identity::generate().expect("identity")).await;
        let err = stranger
            .join("Stranger")
            .await
            .expect_err("a require server must refuse the uncertified");
        assert!(
            matches!(
                err,
                bot::BotError::Refused {
                    reason: DisconnectReason::NameRequired
                }
            ),
            "expected NameRequired, got {err}"
        );

        // The same server with a valid certificate is the public-server shape.
        let identity = Identity::generate().expect("identity");
        let certificate = certificate(&registry, &identity, "iridesium", "Iridesium");
        let mut holder = connect(&server, identity).await;
        holder
            .join_with_certificate("anything", &certificate)
            .await
            .expect("a certified join must be let in");
        holder.disconnect().await;
    });

    server.stop();
}

#[test]
fn off_ignores_a_certificate_a_server_has_the_key_for() {
    // A LAN world pins nothing in practice; this server pins the key on
    // purpose, to prove the policy — not the absence of a key — is what turns
    // the check off.
    let (registry, trust) = registry();
    let server = start("off", NamePolicy::Off, Some(trust), None);
    let identity = Identity::generate().expect("identity");
    let uuid = identity.uuid_as_root();
    let certificate = certificate(&registry, &identity, "alice", "Alice");

    block_on(async {
        let mut bot = connect(&server, identity).await;
        bot.join_with_certificate("Bob", &certificate)
            .await
            .expect("join");

        let identities = server.shared().identities.lock().await;
        assert_eq!(
            identities.name_of(&uuid),
            Some("Bob"),
            "with the policy off, the certificate is not looked at"
        );
        assert_eq!(identities.name_holder("Alice"), None);
        drop(identities);
        bot.disconnect().await;
    });

    server.stop();
}

/// A mod that reports both halves of the mod API's question about a name.
fn marks_maker(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("marks");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"marks\"\nname = \"Marks\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        "game.register_on_player_join(function(e)\n\
         \x20   game.chat_to(e.player, \"cert=\" .. tostring(e.certified)\n\
         \x20       .. \" lookup=\" .. tostring(game.player_certified(e.player)))\n\
         end)\n",
    )
    .expect("script");
    root
}

#[test]
fn the_join_hook_and_the_game_api_agree_and_follow_a_rejoin() {
    let (registry, trust) = registry();
    let server = start(
        "mod-api",
        NamePolicy::Honour,
        Some(trust),
        Some(marks_maker("mod-api-mods")),
    );

    block_on(async {
        // Uncertified: both say false.
        let mut first = connect(&server, Identity::generate().expect("identity")).await;
        first.join("Plain").await.expect("join");
        let told = server_says(&mut first, "cert=false").await;
        assert!(told.contains("lookup=false"), "got {told}");
        first.disconnect().await;

        // Certified: both say true, and the certificate's casing is the name.
        let identity = Identity::generate().expect("identity");
        let seed = identity.seed();
        let uuid = identity.uuid_as_root();
        let certificate = certificate(&registry, &identity, "alice", "Alice");
        let mut certified = connect(&server, identity).await;
        certified
            .join_with_certificate("Whatever", &certificate)
            .await
            .expect("join");
        let told = server_says(&mut certified, "cert=true").await;
        assert!(told.contains("lookup=true"), "got {told}");
        certified.disconnect().await;

        // Same identity, no certificate: the answer is read live, so the mark
        // does not linger from the last join.
        let mut uncertified = connect(&server, Identity::from_seed(&seed)).await;
        uncertified.join("Plain again").await.expect("rejoin");
        let told = server_says(&mut uncertified, "cert=false").await;
        assert!(told.contains("lookup=false"), "got {told}");
        uncertified.disconnect().await;

        let identities = server.shared().identities.lock().await;
        assert_eq!(identities.name_of(&uuid), Some("Plain again"));
    });

    server.stop();
}
