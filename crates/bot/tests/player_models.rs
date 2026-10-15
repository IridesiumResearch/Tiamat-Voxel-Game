// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Life ask 23, part 1: players are drawn as a mod's bodies, in its skins,
//! over a real server.
//!
//! What is measured is the wire — the model and the skin hash on another
//! player's mirror as a second bot sees them, at spawn and when they change —
//! because that is what every client draws from. The client's half, a pass
//! per look, is tested where the renderer is.

use std::path::PathBuf;

use bot::Bot;
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::interest::ViewDistance;
use tiamat_server::{ServerHandle, Settings};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamat-player-models").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Bytes standing in for a model and three skins. The server hashes files;
/// it never decodes them, so these need only exist.
const MALE_GLB: &[u8] = b"glTF male";
const FEMALE_GLB: &[u8] = b"glTF female";
const MALE_DEFAULT: &[u8] = b"PNG male";
const MALE_PALE: &[u8] = b"PNG male pale";
const FEMALE_DEFAULT: &[u8] = b"PNG female";

/// Two bodies, the mod's, and a chat word to dress a player in one or skin
/// them; and a mod that registers a body of its own AFTER, whose body is the
/// world's theme — the last to register any.
fn write_mods(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("bodies");
    std::fs::create_dir_all(dir.join("models")).expect("mod dir");
    std::fs::create_dir_all(dir.join("skins")).expect("skins dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"bodies\"\nname = \"Bodies\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(dir.join("models/male.glb"), MALE_GLB).expect("glb");
    std::fs::write(dir.join("models/female.glb"), FEMALE_GLB).expect("glb");
    std::fs::write(dir.join("models/male.png"), MALE_DEFAULT).expect("png");
    std::fs::write(dir.join("skins/male_pale.png"), MALE_PALE).expect("png");
    std::fs::write(dir.join("models/female.png"), FEMALE_DEFAULT).expect("png");
    std::fs::write(
        dir.join("init.lua"),
        r#"
local ground = game.register_block{ id = "ground" }
game.register_on_generate(function(buf, pos)
    buf:fill_below_heightmap(game.flat_heightmap(0), ground)
end)
game.register_model{ id = "male", file = "models/male.glb", texture = "models/male.png" }
game.register_model{ id = "female", file = "models/female.glb", texture = "models/female.png" }
game.register_player_model{ id = "male", model = "male", skins = { "skins/male_pale.png", "skins/missing.png" } }
game.register_player_model{ id = "female", model = "bodies:female" }
-- A typo is an error at registration, not a body nobody can wear.
assert(not pcall(game.register_player_model, { id = "x", model = "male", skin = {} }))
assert(not pcall(game.register_player_model, { id = "a:b", model = "male" }))

game.register_on_chat(function(event)
    local words = {}
    for word in event.text:gmatch("%S+") do words[#words + 1] = word end
    if words[1] == "wear" then
        local ok, err = pcall(game.set_player_model, event.player, words[2])
        game.chat_to(event.player, "wear " .. tostring(ok) .. " " .. tostring(err))
        return ""
    elseif words[1] == "skin" then
        local ok, err = pcall(game.set_player_skin, event.player, words[2])
        game.chat_to(event.player, "skin " .. tostring(ok) .. " " .. tostring(err))
        return ""
    elseif words[1] == "undress" then
        game.set_player_model(event.player, nil)
        game.chat_to(event.player, "undress true")
        return ""
    end
end)
"#,
    )
    .expect("script");
    root
}

fn start(name: &str) -> ServerHandle {
    ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: scratch(&format!("{name}-world")),
        identity_path: None,
        max_players: 3,
        allow_custom_skins: false,
        sticky_names: false,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(write_mods(&format!("{name}-mods"))),
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

async fn connect(server: &ServerHandle, name: &str) -> Bot {
    let mut bot = Bot::connect(
        server.local_addr(),
        Identity::generate().expect("identity"),
        server.cert_fingerprint(),
    )
    .await
    .expect("connect");
    bot.join(name).await.expect("join");
    bot.sleep_ticks(10).await;
    bot
}

fn notice_after(bot: &Bot, seen: usize, prefix: &str) -> Option<String> {
    bot.notices()
        .into_iter()
        .skip(seen)
        .rev()
        .find_map(|text| text.strip_prefix(prefix).map(str::to_owned))
}

/// Says `text` and answers the mod's reply, which starts with its first word.
async fn ask(bot: &mut Bot, text: &str) -> String {
    let prefix = format!("{} ", text.split_whitespace().next().expect("a word"));
    let seen = bot.notices().len();
    bot.chat(text).await.expect("chat");
    for _ in 0..40 {
        if let Some(answer) = notice_after(bot, seen, &prefix) {
            return answer;
        }
        bot.walk([0.0; 3], 0, 1).await.expect("wait");
    }
    panic!("the mod never answered `{text}`");
}

/// The model and the skin on the one player's mirror `watcher` can see.
async fn seen_look(watcher: &mut Bot) -> (Option<String>, Option<[u8; 32]>) {
    for _ in 0..40 {
        watcher.walk([0.0; 3], 0, 1).await.expect("wait");
        let players: Vec<_> = watcher
            .entities()
            .into_values()
            .filter(|entity| entity.nametag.is_some())
            .collect();
        if let Some(player) = players.first() {
            return (player.model.clone(), player.skin);
        }
    }
    panic!("the watcher never saw the other player");
}

/// Waits until the one player `watcher` sees is drawn as `want`.
async fn seen_look_becomes(
    watcher: &mut Bot,
    want: (Option<String>, Option<[u8; 32]>),
) -> (Option<String>, Option<[u8; 32]>) {
    let mut seen = seen_look(watcher).await;
    for _ in 0..60 {
        if seen == want {
            break;
        }
        watcher.walk([0.0; 3], 0, 2).await.expect("wait");
        seen = seen_look(watcher).await;
    }
    seen
}

#[test]
fn a_player_is_drawn_as_the_mods_first_body_and_as_the_one_a_mod_dresses_them_in() {
    let server = start("bodies");
    block_on(async {
        let mut dressed = connect(&server, "Dressed").await;
        let mut watcher = connect(&server, "Watcher").await;

        // At spawn: the first body registered, in the model's own texture.
        let seen = seen_look(&mut watcher).await;
        assert_eq!(seen, (Some("bodies:male".to_owned()), None));

        // Dressed in the other body: the watcher's copy is told, reliably.
        assert_eq!(ask(&mut dressed, "wear female").await, "true true");
        let seen = seen_look_becomes(&mut watcher, (Some("bodies:female".to_owned()), None)).await;
        assert_eq!(seen, (Some("bodies:female".to_owned()), None));

        // A body nobody registered is an error, and changes nothing.
        let answer = ask(&mut dressed, "wear elf").await;
        assert!(answer.starts_with("false "), "{answer}");
        assert!(answer.contains("elf"), "{answer}");

        // A skin, on the body it was declared for.
        assert_eq!(ask(&mut dressed, "wear male").await, "true true");
        assert_eq!(
            ask(&mut dressed, "skin skins/male_pale.png").await,
            "true true"
        );
        let pale = tiamat_core::content::hash_bytes(MALE_PALE);
        let seen =
            seen_look_becomes(&mut watcher, (Some("bodies:male".to_owned()), Some(pale))).await;
        assert_eq!(seen, (Some("bodies:male".to_owned()), Some(pale)));

        // A skin the body did not declare, and one declared but missing from
        // the directory, are both refused.
        let answer = ask(&mut dressed, "skin skins/other.png").await;
        assert!(answer.starts_with("false "), "{answer}");
        let answer = ask(&mut dressed, "skin skins/missing.png").await;
        assert!(answer.starts_with("false "), "{answer}");

        // Changing body takes the skin off; `nil` is the first body again.
        assert_eq!(ask(&mut dressed, "wear female").await, "true true");
        let seen = seen_look_becomes(&mut watcher, (Some("bodies:female".to_owned()), None)).await;
        assert_eq!(
            seen.1, None,
            "a skin painted for one body stayed on another"
        );
        assert_eq!(ask(&mut dressed, "undress").await, "true");
        let seen = seen_look_becomes(&mut watcher, (Some("bodies:male".to_owned()), None)).await;
        assert_eq!(seen, (Some("bodies:male".to_owned()), None));

        // And the dressed player is told their own look, for third person.
        let own: Vec<(Option<String>, Option<[u8; 32]>)> = dressed
            .received()
            .into_iter()
            .filter_map(|message| match message {
                tiamat_core::proto::ServerMessage::PlayerLook { model, skin } => {
                    Some((model, skin))
                }
                _ => None,
            })
            .collect();
        assert!(
            own.contains(&(Some("bodies:male".to_owned()), Some(pale))),
            "the player was never told about their own skin: {own:?}"
        );
        assert_eq!(
            own.last(),
            Some(&(Some("bodies:male".to_owned()), None)),
            "the last word on their own look is the first body, bare"
        );

        dressed.disconnect().await;
        watcher.disconnect().await;
    });
    assert!(server.stop());
}
