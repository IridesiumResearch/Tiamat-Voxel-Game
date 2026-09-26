// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The reference build tool, over a real server.
//!
//! `plans.rs` holds the ENGINE to its promises about a plan: the caps, the
//! paced stamp, the survival of a restart. This holds `game/core_plans` to
//! its five chat words, from where a player stands, through nothing but the
//! public API.

use std::path::{Path, PathBuf};
use std::time::Duration;

use bot::Bot;
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::interest::ViewDistance;
use tiamat_core::{BlockPos, SubNodePos};
use tiamat_server::{ServerHandle, Settings};

/// How long to wait for something the server has to tick before it is true.
const PATIENCE: Duration = Duration::from_secs(30);

fn centre_of(pos: BlockPos) -> SubNodePos {
    SubNodePos::new(pos.x * 3 + 1, pos.y * 3 + 1, pos.z * 3 + 1)
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamat-core-plans").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("copy dir");
    for entry in std::fs::read_dir(from).expect("read dir") {
        let entry = entry.expect("dir entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

/// The reference build tool beside a yard: flat ground, a hand, and bricks
/// in every joiner's pack to build something worth copying.
fn write_mods(root: &Path) -> PathBuf {
    let mods = root.join("mods");
    let repo_game = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../game");
    copy_tree(&repo_game.join("core_plans"), &mods.join("core_plans"));
    let yard = mods.join("yard");
    std::fs::create_dir_all(&yard).expect("mod dir");
    std::fs::write(
        yard.join("mod.toml"),
        "id = \"yard\"\nname = \"Yard\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        yard.join("init.lua"),
        r#"
local ground = game.register_block{ id = "ground" }
game.register_block{ id = "brick" }
game.register_on_generate(function(buf, pos)
    buf:fill_below_heightmap(game.flat_heightmap(0), ground)
end)
game.register_tool{ id = "hand", brush = "block", speed_multiplier = 1.0, default = true }
game.register_on_player_join(function(event)
    game.give(event.player, { material = "yard:brick", count = 4 })
end)
"#,
    )
    .expect("script");
    mods
}

fn start(name: &str) -> ServerHandle {
    let root = scratch(name);
    let mods = write_mods(&root);
    ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: root.join("world"),
        identity_path: None,
        max_players: 4,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(mods),
        enabled_mods: None,
        seed: Some(19),
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

async fn join(server: &ServerHandle, name: &str) -> Bot {
    let mut bot = Bot::connect(
        server.local_addr(),
        Identity::generate().expect("identity"),
        server.cert_fingerprint(),
    )
    .await
    .expect("connect");
    bot.join(name).await.expect("join");
    bot
}

fn material_id(bot: &Bot, name: &str) -> u16 {
    bot.material_table()
        .expect("a material table on join")
        .iter()
        .find(|def| def.name == name)
        .map(|def| def.id)
        .unwrap_or_else(|| panic!("no material called {name}"))
}

async fn until(bot: &mut Bot, done: impl Fn(&Bot) -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while tokio::time::Instant::now() < deadline {
        if done(bot) {
            return true;
        }
        let _ = tokio::time::timeout(Duration::from_millis(200), bot.recv()).await;
    }
    done(bot)
}

fn told(bot: &Bot, word: &str) -> bool {
    bot.notices().iter().any(|text| text.starts_with(word))
}

/// Says a word and waits for the answer that starts with `reply`.
async fn ask(bot: &mut Bot, word: &str, reply: &str) {
    bot.chat(word).await.expect("send");
    assert!(
        until(bot, |bot| told(bot, reply)).await,
        "`{word}` was not answered `{reply}…`; notices {:?}",
        bot.notices()
    );
}

#[test]
fn a_player_captures_a_build_by_chat_and_stamps_it_somewhere_else() {
    let server = start("words");
    block_on(async {
        let mut bot = join(&server, "Mason").await;
        let brick = material_id(&bot, "yard:brick");
        assert!(
            until(&mut bot, |bot| bot.units_of(brick) >= 54).await,
            "the yard should hand out bricks; inventory {:?}",
            bot.inventory()
        );

        // Two bricks in a row, then captured by name: two whole blocks is 54
        // units, and the answer says so (charter rule 5).
        for x in [2, 3] {
            bot.place_from_inventory(centre_of(BlockPos::new(x, 0, 0)), brick)
                .await
                .expect("send");
            bot.expect_block(BlockPos::new(x, 0, 0), brick, PATIENCE)
                .await
                .expect("the brick should land");
        }
        ask(&mut bot, "capture hut 2 0 0 3 0 0", "captured hut").await;
        assert!(
            told(&bot, "captured hut: 2x1x1, 2 blocks, yard:brick=54"),
            "the summary is wrong; notices {:?}",
            bot.notices()
        );
        ask(&mut bot, "plans", "plans: hut").await;
        ask(&mut bot, "plan hut", "hut: 2x1x1, 2 blocks, yard:brick=54").await;

        // Stamped ten blocks along: the bricks appear there, paced by the
        // engine, and the originals are untouched.
        ask(&mut bot, "stamp hut 10 0 0", "stamping hut at 10,0,0").await;
        for x in [10, 11] {
            bot.expect_block(BlockPos::new(x, 0, 0), brick, PATIENCE)
                .await
                .expect("the stamp should land");
        }
        assert!(bot.saw_block(BlockPos::new(2, 0, 0), brick));

        // Forgotten, and gone from the list.
        ask(&mut bot, "forget hut", "forgot hut").await;
        ask(&mut bot, "plans", "no plans").await;
        ask(&mut bot, "stamp hut 20 0 0", "cannot stamp hut").await;

        // A box in terrain nobody has loaded is refused whole, in words.
        ask(
            &mut bot,
            "capture far 5000 0 5000 5001 0 5001",
            "cannot capture far: not loaded",
        )
        .await;
        // And a word with the wrong shape is answered with the right one.
        ask(&mut bot, "capture", "capture <name>").await;
    });
    assert!(server.stop());
}
