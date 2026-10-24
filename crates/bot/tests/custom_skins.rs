// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Life ask 23, the last piece: a skin a player chose for their own body,
//! taken by a host who allows it and passed on to everybody who sees them.
//!
//! The wire is what is measured — the skin hash on the player's mirror as a
//! second bot sees it, the join hook's `skin`, the refusals as the player
//! hears them — over a real server with the switch on and one with it off.

use std::path::PathBuf;

use bot::Bot;
use tiamat_core::content::{hash_bytes, png_header};
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::interest::ViewDistance;
use tiamat_server::{ServerHandle, Settings};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamat-custom-skins").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// One body whose texture is painted 64 by 32 — the template a player's own
/// skin must match — and a mod that reports the join hook's `skin`, the
/// host's switch, and can put its own skin on or take every skin off.
fn write_mod(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("bodies");
    std::fs::create_dir_all(dir.join("models")).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"bodies\"\nname = \"Bodies\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(dir.join("models/male.glb"), b"glTF male").expect("glb");
    std::fs::write(dir.join("models/male.png"), png_header(64, 32)).expect("template");
    std::fs::write(dir.join("models/male_pale.png"), png_header(64, 32)).expect("skin");
    std::fs::write(
        dir.join("init.lua"),
        r#"
local ground = game.register_block{ id = "ground" }
game.register_on_generate(function(buf, pos)
    buf:fill_below_heightmap(game.flat_heightmap(0), ground)
end)
game.register_model{ id = "male", file = "models/male.glb", texture = "models/male.png" }
game.register_player_model{ id = "male", model = "male", skins = { "models/male_pale.png" } }

game.register_on_player_join(function(event)
    game.chat_to(event.player, "joined " .. tostring(event.skin) .. " " .. tostring(game.custom_skins_allowed()))
end)
game.register_on_chat(function(event)
    if event.text == "pale" then
        game.set_player_skin(event.player, "models/male_pale.png")
        return ""
    elseif event.text == "bare" then
        game.set_player_skin(event.player, nil)
        return ""
    end
end)
"#,
    )
    .expect("script");
    root
}

fn start(name: &str, allow: bool) -> ServerHandle {
    ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: scratch(&format!("{name}-world")),
        identity_path: None,
        max_players: 3,
        allow_custom_skins: allow,
        sticky_names: false,
        name_policy: tiamat_core::session::NamePolicy::Off,
        registry_trust: None,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(write_mod(&format!("{name}-mods"))),
        enabled_mods: None,
        seed: Some(5),
        rcon: None,
        materials: Vec::new(),
        world_options: Vec::new(),
    })
    .expect("start")
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(future)
}

async fn connect(server: &ServerHandle) -> Bot {
    Bot::connect(
        server.local_addr(),
        Identity::generate().expect("identity"),
        server.cert_fingerprint(),
    )
    .await
    .expect("connect")
}

fn hex(hash: [u8; 32]) -> String {
    hash.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The first notice after `seen` starting with `prefix`, waiting a little.
async fn notice(bot: &mut Bot, seen: usize, prefix: &str) -> String {
    for _ in 0..60 {
        if let Some(text) = bot
            .notices()
            .into_iter()
            .skip(seen)
            .find_map(|text| text.strip_prefix(prefix).map(str::to_owned))
        {
            return text;
        }
        bot.walk([0.0; 3], 0, 1).await.expect("wait");
    }
    panic!("no notice `{prefix}…` arrived");
}

/// The skin on the one player's mirror `watcher` sees, once it is `want`
/// or the patience runs out.
async fn seen_skin(watcher: &mut Bot, want: Option<[u8; 32]>) -> Option<[u8; 32]> {
    let mut seen = None;
    for _ in 0..80 {
        watcher.walk([0.0; 3], 0, 2).await.expect("wait");
        seen = watcher
            .entities()
            .into_values()
            .find(|entity| entity.nametag.is_some())
            .map(|entity| entity.skin);
        if seen == Some(want) {
            break;
        }
    }
    seen.expect("the watcher never saw the other player")
}

#[test]
fn a_host_who_allows_it_passes_a_players_own_skin_on_and_the_join_hook_sees_it() {
    let server = start("allowed", true);
    block_on(async {
        let own = png_header(64, 32);
        let own_hash = hash_bytes(&own);
        let mut wearer = connect(&server).await;
        wearer
            .join_with_skins("Wearer", &[(None, own.clone())])
            .await
            .expect("join");
        wearer.sleep_ticks(10).await;
        assert_eq!(wearer.custom_skins(), Some(true));
        // The join hook saw the skin, and the host's switch.
        let joined = notice(&mut wearer, 0, "joined ").await;
        assert_eq!(joined, format!("{} true", hex(own_hash)));

        let mut watcher = connect(&server).await;
        watcher.join("Watcher").await.expect("join");
        assert_eq!(
            seen_skin(&mut watcher, Some(own_hash)).await,
            Some(own_hash)
        );

        // A mod's say wins over it, and `nil` is the model's own texture,
        // not the player's skin back.
        let pale = hash_bytes(&png_header(64, 32));
        assert_eq!(
            pale, own_hash,
            "the fixture's skins are the same header; the mod's is told apart by its say"
        );
        wearer.chat("bare").await.expect("chat");
        assert_eq!(seen_skin(&mut watcher, None).await, None);

        // A skin the wrong size is kept but not worn, with a word.
        let seen = wearer.notices().len();
        wearer
            .offer_skin(None, png_header(16, 16))
            .await
            .expect("offer");
        let word = notice(&mut wearer, seen, "your skin is ").await;
        assert!(
            word.contains("16 by 16") && word.contains("64 by 32"),
            "{word}"
        );
        // Not a PNG at all: refused, with a word.
        let seen = wearer.notices().len();
        wearer
            .offer_skin(None, b"not a picture".to_vec())
            .await
            .expect("offer");
        let word = notice(&mut wearer, seen, "your skin is ").await;
        assert!(word.contains("not a PNG"), "{word}");
        // A body this world does not have.
        let seen = wearer.notices().len();
        wearer
            .offer_skin(Some("elf".to_owned()), png_header(64, 32))
            .await
            .expect("offer");
        let word = notice(&mut wearer, seen, "your skin is ").await;
        assert!(word.contains("elf"), "{word}");

        wearer.disconnect().await;
        watcher.disconnect().await;
    });
    assert!(server.stop());
}

#[test]
fn a_host_who_does_not_allow_it_takes_nothing_and_says_so() {
    let server = start("refused", false);
    block_on(async {
        let own = png_header(64, 32);
        let mut wearer = connect(&server).await;
        wearer
            .join_with_skins("Wearer", &[(None, own)])
            .await
            .expect("join");
        wearer.sleep_ticks(10).await;
        assert_eq!(wearer.custom_skins(), Some(false));
        let joined = notice(&mut wearer, 0, "joined ").await;
        assert_eq!(joined, "nil false");
        let word = notice(&mut wearer, 0, "this host ").await;
        assert!(word.contains("does not allow"), "{word}");

        let mut watcher = connect(&server).await;
        watcher.join("Watcher").await.expect("join");
        assert_eq!(seen_skin(&mut watcher, None).await, None);
        wearer.disconnect().await;
        watcher.disconnect().await;
    });
    assert!(server.stop());
}
