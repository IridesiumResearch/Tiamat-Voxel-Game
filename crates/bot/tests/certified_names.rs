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
use tiamat_core::identity::keyset::commit_to;
use tiamat_core::identity::registry::{
    Name, NameCertificate, ROTATION_VERSION, RegistryTrust, RotationRecord, Signature64,
};
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
///
/// **Thirty seconds, like the bot's own reads, not ten.** The deadline is here
/// to end a hang, not to time anything: on a loaded macOS runner the join hook
/// runs a tick or two after `JoinWorld` and a short deadline made this test
/// flaky there. `stage` names which join went quiet when it does.
async fn server_says(bot: &mut Bot, needle: &str, stage: &str) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !left.is_zero(),
            "the server never said `{needle}` ({stage})"
        );
        let message = match tokio::time::timeout(left, bot.recv()).await {
            Ok(Ok(message)) => message,
            Ok(Err(err)) => panic!("the bot failed waiting for `{needle}` ({stage}): {err}"),
            Err(_) => panic!("the server went quiet waiting for `{needle}` ({stage})"),
        };
        if let ServerMessage::Chat { from: None, text } = message
            && text.contains(needle)
        {
            return text;
        }
    }
}

/// Waits until the server has let go of a departed session.
///
/// A reconnect in the same breath as a disconnect is the quick-reconnect race
/// the server's slot generations address; it is not what the mod API test is
/// about, and racing it made the test flaky on a slow runner. Waiting for the
/// session to be gone tests the thing it names.
async fn wait_until_offline(server: &ServerHandle, uuid: tiamat_core::identity::PlayerUuid) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while server
        .shared()
        .online_players()
        .iter()
        .any(|(who, _)| *who == uuid)
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the server kept a departed session in its roster for ten seconds"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Waits until the simulation has had `ticks` ticks to notice a departure.
///
/// The roster entry goes the moment the connection task ends; the simulation
/// that fires `on_player_join` learns the player left from the next tick's
/// presence diff. A reconnect between the two is a player who never left as
/// far as the hooks are concerned — which is the race this test does not mean
/// to measure. Two ticks is one to see the absence and one to be safely past
/// it.
async fn wait_ticks(server: &ServerHandle, ticks: u64) {
    let start = server.control().tick();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while server.control().tick() < start.saturating_add(ticks) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the server's tick did not advance for ten seconds"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
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
        let told = server_says(
            &mut alice,
            "registered to another identity",
            "the displaced holder",
        )
        .await;
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
        let told = server_says(
            &mut carol,
            "already somebody else's name",
            "the uncertified claimant",
        )
        .await;
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
        let told = server_says(&mut first, "cert=false", "the first uncertified join").await;
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
        let told = server_says(&mut certified, "cert=true", "the certified join").await;
        assert!(told.contains("lookup=true"), "got {told}");
        certified.disconnect().await;
        // The departing session's slot is released asynchronously; a rejoin in
        // the same breath is the quick-reconnect race, not the mod API. Give
        // the simulation two ticks to see them gone as well as the roster.
        wait_until_offline(&server, uuid).await;
        wait_ticks(&server, 2).await;

        // Same identity, no certificate: the answer is read live, so the mark
        // does not linger from the last join.
        let mut uncertified = connect(&server, Identity::from_seed(&seed)).await;
        uncertified.join("Plain again").await.expect("rejoin");
        let told = server_says(
            &mut uncertified,
            "cert=false",
            "the rejoin after a certified session",
        )
        .await;
        assert!(told.contains("lookup=false"), "got {told}");
        uncertified.disconnect().await;

        let identities = server.shared().identities.lock().await;
        assert_eq!(identities.name_of(&uuid), Some("Plain again"));
    });

    server.stop();
}

/// The registry before and after a rotation: the old key, the new one, the
/// record the old key signed to hand over, and what a server built before the
/// rotation pins — the old key and the hash of the new one (design §5).
struct Rotated {
    new: ed25519_dalek::SigningKey,
    record: Vec<u8>,
    pin: RegistryTrust,
}

fn rotated() -> Rotated {
    let old = ed25519_dalek::SigningKey::from_bytes(&[0x5B; 32]);
    let new = ed25519_dalek::SigningKey::from_bytes(&[0x5C; 32]);
    let mut record = RotationRecord {
        version: ROTATION_VERSION,
        retired: [0; 32],
        successor: new.verifying_key().to_bytes(),
        next_key_hash: None,
        rotated_at: unix_now() - 60,
        signature: Signature64([0; 64]),
    };
    record.sign(&old);
    let record = record.to_bytes().expect("a record encodes");
    let pin = RegistryTrust::new(old.verifying_key(), Some(commit_to(&new.verifying_key())));
    Rotated { new, record, pin }
}

#[test]
fn a_registry_rotation_reaches_a_server_pinning_the_old_key_with_the_player() {
    let rotated = rotated();
    let server = start(
        "rotated-honour",
        NamePolicy::Honour,
        Some(rotated.pin.clone()),
        None,
    );
    let warned = || {
        server
            .shared()
            .registry_rotation_warned
            .load(std::sync::atomic::Ordering::Acquire)
    };

    block_on(async {
        // Renewed under the new key, carrying the record: certified.
        let identity = Identity::generate().expect("identity");
        let uuid = identity.uuid_as_root();
        let renewed = certificate(&rotated.new, &identity, "alice", "Alice");
        assert!(!warned(), "nobody has shown this server a rotation yet");
        let mut alice = connect(&server, identity).await;
        alice
            .join_with_certificate_and_rotations(
                "anything",
                &renewed,
                std::slice::from_ref(&rotated.record),
            )
            .await
            .expect("a certified join");
        assert!(server.shared().is_certified(&uuid));
        assert!(warned(), "the operator is told the pin is behind");
        {
            let identities = server.shared().identities.lock().await;
            assert_eq!(identities.name_of(&uuid), Some("Alice"));
            assert!(identities.is_certified_name("Alice"));
        }

        // The same kind of certificate without the record: the new key is a
        // stranger to this server, so an ordinary join under `honour`.
        let identity = Identity::generate().expect("identity");
        let uuid = identity.uuid_as_root();
        let renewed = certificate(&rotated.new, &identity, "bob", "Bob");
        let mut bob = connect(&server, identity).await;
        bob.join_with_certificate("Plain", &renewed)
            .await
            .expect("an uncertified join");
        assert!(!server.shared().is_certified(&uuid));
        {
            let identities = server.shared().identities.lock().await;
            assert_eq!(identities.name_of(&uuid), Some("Plain"));
            assert_eq!(identities.name_holder("Bob"), None);
        }

        bob.disconnect().await;
        alice.disconnect().await;
    });
    server.stop();

    // A public server that pins the same old key: the record is what lets the
    // renewed player in at all.
    let server = start(
        "rotated-require",
        NamePolicy::Require,
        Some(rotated.pin),
        None,
    );
    block_on(async {
        let identity = Identity::generate().expect("identity");
        let seed = identity.seed();
        let renewed = certificate(&rotated.new, &identity, "carol", "Carol");
        let mut carol = connect(&server, identity).await;
        let err = carol
            .join_with_certificate("Carol", &renewed)
            .await
            .expect_err("no record, no trusted key, no join");
        assert!(
            matches!(
                err,
                bot::BotError::Refused {
                    reason: DisconnectReason::NameRequired
                }
            ),
            "expected NameRequired, got {err}"
        );

        let mut carol = connect(&server, Identity::from_seed(&seed)).await;
        carol
            .join_with_certificate_and_rotations(
                "Carol",
                &renewed,
                std::slice::from_ref(&rotated.record),
            )
            .await
            .expect("with the record, admitted");
        carol.disconnect().await;
    });
    server.stop();
}
