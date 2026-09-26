// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! What a world keeps across a clean quit and a relaunch.
//!
//! Singleplayer is the embedded server, so quitting the game stops it and
//! starting again opens the same world on a new one. Two things were lost on
//! the way: the hour — the clock was seeded from the sky mod's `start_time`
//! on every start, so a night ended at every launch — and the mods' leave
//! hook, which the tick fires only for somebody it SAW go, which nobody is
//! when the server stops under them. The World mod writes a player's exact
//! position in that hook; without it a player who quit up a tree came back on
//! the ground under it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use bot::Bot;
use tiamat_core::BlockPos;
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::interest::ViewDistance;
use tiamat_server::{ServerHandle, Settings};

const PATIENCE: Duration = Duration::from_secs(30);

/// Where the mod says a leave hook ran, once the world is back.
const LEFT: BlockPos = BlockPos::new(2, 10, 2);

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamat-clock").join(name);
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

/// The reference sky, so the world has a day, beside a mod that can set the
/// hour by a word, remembers a leave in its storage, and says so with a block
/// once the world is back.
fn write_mods(name: &str) -> PathBuf {
    let mods = scratch(name);
    let repo_game = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../game");
    copy_tree(&repo_game.join("core_sky"), &mods.join("core_sky"));
    let dir = mods.join("clockwork");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"clockwork\"\nname = \"Clockwork\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        r#"
local ground = game.register_block{ id = "ground" }
game.register_block{ id = "mark" }
game.register_on_generate(function(buf, pos)
    buf:fill_below_heightmap(game.flat_heightmap(0), ground)
end)
game.register_on_chat(function(event)
    if event.text == "evening" then
        game.set_time_of_day(0.9)
        return false
    end
end)
game.register_on_player_leave(function(event)
    game.storage.set("left", "yes")
end)
game.register_on_tick(function()
    if game.storage.get("left") == "yes" then
        game.set_block({ x = 2, y = 10, z = 2 }, "clockwork:mark")
    end
end)
"#,
    )
    .expect("script");
    mods
}

fn start(mods: PathBuf, world: PathBuf) -> ServerHandle {
    ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: world,
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

fn mark_id(bot: &Bot) -> u16 {
    bot.material_table()
        .expect("a material table on join")
        .into_iter()
        .find(|entry| entry.name == "clockwork:mark")
        .map(|entry| entry.id)
        .expect("the mod registers a mark")
}

#[test]
fn the_hour_survives_a_clean_quit_and_a_relaunch() {
    let world = scratch("hour-world");
    let first = start(write_mods("hour-first"), world.clone());
    block_on(async {
        let mut bot = join(&first, "Owl").await;
        bot.chat("evening").await.expect("send");
        assert!(
            until(&mut bot, |bot| bot
                .time_of_day()
                .is_some_and(|time| time > 0.85))
            .await,
            "the word did not set the hour; last {:?}",
            bot.time_of_day()
        );
        bot.disconnect().await;
    });
    assert!(first.stop(), "the world did not flush cleanly");

    let second = start(write_mods("hour-second"), world);
    block_on(async {
        // A fresh identity, so a fresh name: the first one is bound to the
        // identity that claimed it, which is not what a test proves here.
        let mut bot = join(&second, "Lark").await;
        assert!(
            until(&mut bot, |bot| bot.time_of_day().is_some()).await,
            "no time of day arrived after the relaunch"
        );
        let time = bot.time_of_day().expect("a time");
        assert!(
            (0.85..0.99).contains(&time),
            "the world came back at {time}, not at the evening it was left: the clock was \
             seeded from the sky's start_time again"
        );
    });
    assert!(second.stop());
}

#[test]
fn a_clean_shutdown_lets_the_mods_see_everybody_leave() {
    let world = scratch("leave-world");
    let first = start(write_mods("leave-first"), world.clone());
    block_on(async {
        let mut bot = join(&first, "Climber").await;
        bot.sleep_ticks(4).await;
        // Still connected when the server stops: the case the tick's own
        // leave-diff never sees.
        bot.abandon();
    });
    assert!(first.stop(), "the world did not flush cleanly");

    let second = start(write_mods("leave-second"), world);
    block_on(async {
        let mut bot = join(&second, "Walker").await;
        let mark = mark_id(&bot);
        bot.expect_block(LEFT, mark, PATIENCE)
            .await
            .expect("the leave hook should have run at shutdown and its storage survived");
    });
    assert!(second.stop());
}
