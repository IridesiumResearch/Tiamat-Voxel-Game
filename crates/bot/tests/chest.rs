// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The reference chest, over a real server.
//!
//! `containers.rs` holds the ENGINE to its promises about a container: one
//! holder at a time, the contents surviving a disconnect and a restart. This
//! holds `game/core_chest` to the three steps a chest mod takes — made when
//! placed, opened when used, emptied into the digger's hands when dug —
//! through nothing but the public API, which is what makes it a reference.

use std::path::{Path, PathBuf};
use std::time::Duration;

use bot::Bot;
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::interest::ViewDistance;
use tiamat_core::proto::{Click, DialogEvent};
use tiamat_core::{BlockPos, SubNodePos};
use tiamat_server::{ServerHandle, Settings};

/// How long to wait for something the server has to tick before it is true.
///
/// Thirty seconds, for the reason `containers.rs` gives: every wait ends on
/// its condition, and a shorter clock is a bet on the machine.
const PATIENCE: Duration = Duration::from_secs(30);

/// The chest's screen, as `game/core_chest/init.lua` names it.
const FORM: &str = "core_chest:chest";

/// The container behind the chest every test here places, as the mod names
/// it: by where it stands.
const CHEST: &str = "core_chest:at:2,0,0";

/// Where that chest stands: two blocks from where a player joins, in reach.
fn chest_at() -> BlockPos {
    BlockPos::new(2, 0, 0)
}

fn centre_of(pos: BlockPos) -> SubNodePos {
    SubNodePos::new(pos.x * 3 + 1, pos.y * 3 + 1, pos.z * 3 + 1)
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamat-chest").join(name);
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

/// The reference chest beside a yard to stand in: flat ground, a hand to dig
/// with, and two chests in every joiner's pack — one to place, one to put in
/// it.
fn write_mods(root: &Path) -> PathBuf {
    let mods = root.join("mods");
    let repo_game = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../game");
    copy_tree(&repo_game.join("core_chest"), &mods.join("core_chest"));
    let yard = mods.join("yard");
    std::fs::create_dir_all(&yard).expect("mod dir");
    std::fs::write(
        yard.join("mod.toml"),
        "id = \"yard\"\nname = \"Yard\"\nversion = \"0.1.0\"\n\
         license = \"GPL-3.0-only\"\ndepends = [\"core_chest\"]\n",
    )
    .expect("manifest");
    std::fs::write(
        yard.join("init.lua"),
        r#"
local ground = game.register_block{ id = "ground" }
game.register_on_generate(function(buf, pos)
    buf:fill_below_heightmap(game.flat_heightmap(0), ground)
end)
game.register_tool{ id = "hand", brush = "block", speed_multiplier = 1.0, default = true }
game.register_on_player_join(function(event)
    game.give(event.player, { material = "core_chest:chest", count = 2 })
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

/// The numeric id a material has on this server.
fn material_id(bot: &Bot, name: &str) -> u16 {
    bot.material_table()
        .expect("a material table on join")
        .iter()
        .find(|def| def.name == name)
        .map(|def| def.id)
        .unwrap_or_else(|| panic!("no material called {name}"))
}

/// Reads until `done`, or until patience runs out. Answers `done` either way.
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

/// Places a chest from the pack and opens it, the way every test starts.
async fn place_and_open(bot: &mut Bot, chest: u16) {
    assert!(
        until(bot, |bot| bot.units_of(chest) == 54).await,
        "the yard should hand out two chests; inventory {:?}",
        bot.inventory()
    );
    bot.place_from_inventory(centre_of(chest_at()), chest)
        .await
        .expect("send");
    bot.expect_block(chest_at(), chest, PATIENCE)
        .await
        .expect("the chest should land");
    bot.use_block(centre_of(chest_at())).await.expect("send");
    assert!(
        until(bot, |bot| bot
            .dialogs()
            .iter()
            .any(|(form, _)| form == FORM)
            && bot.view(CHEST).is_some())
        .await,
        "no chest screen with the box in it; dialogs {:?}, views {:?}",
        bot.dialogs()
            .iter()
            .map(|(form, _)| form.clone())
            .collect::<Vec<_>>(),
        bot.views().keys().collect::<Vec<_>>()
    );
}

#[test]
fn a_chest_is_placed_opened_filled_and_dug_and_everything_comes_back() {
    let server = start("round-trip");
    block_on(async {
        let mut bot = join(&server, "Keeper").await;
        let chest = material_id(&bot, "core_chest:chest");
        place_and_open(&mut bot, chest).await;

        // Filled: the other chest shift-clicked across, as a player would. The
        // mod moves nothing here; the engine does, because the box is lent
        // into this player's own slots.
        let slot = bot
            .view("player:main")
            .expect("the pack")
            .iter()
            .position(|stack| stack.as_ref().is_some_and(|stack| stack.material == chest))
            .expect("the second chest in the pack");
        bot.dialog_event(
            FORM,
            DialogEvent::Clicked {
                view: "player:main".to_owned(),
                index: u16::try_from(slot).expect("a slot"),
                click: Click::ShiftLeft,
            },
        )
        .await
        .expect("send");
        let in_the_box = |bot: &Bot| {
            bot.view(CHEST)
                .is_some_and(|slots| slots.iter().flatten().any(|stack| stack.material == chest))
        };
        assert!(
            until(&mut bot, in_the_box).await,
            "the chest did not go into the box; box {:?}",
            bot.view(CHEST)
        );

        // And back out with the same gesture from the box's side, then in
        // again: a shift-click crosses in both directions.
        let boxed = bot
            .view(CHEST)
            .expect("the box")
            .iter()
            .position(|stack| stack.as_ref().is_some_and(|stack| stack.material == chest))
            .expect("the chest in the box");
        bot.dialog_event(
            FORM,
            DialogEvent::Clicked {
                view: CHEST.to_owned(),
                index: u16::try_from(boxed).expect("a slot"),
                click: Click::ShiftLeft,
            },
        )
        .await
        .expect("send");
        assert!(
            until(&mut bot, |bot| !in_the_box(bot)
                && bot.units_of(chest) == 27)
            .await,
            "the chest did not come back out; box {:?}, inventory {:?}",
            bot.view(CHEST),
            bot.inventory()
        );
        let slot = bot
            .view("player:main")
            .expect("the pack")
            .iter()
            .position(|stack| stack.as_ref().is_some_and(|stack| stack.material == chest))
            .expect("the chest back in the pack");
        bot.dialog_event(
            FORM,
            DialogEvent::Clicked {
                view: "player:main".to_owned(),
                index: u16::try_from(slot).expect("a slot"),
                click: Click::ShiftLeft,
            },
        )
        .await
        .expect("send");
        assert!(
            until(&mut bot, in_the_box).await,
            "the chest did not go back in; box {:?}",
            bot.view(CHEST)
        );

        // Closed, then dug: the block and what was in it both come back to
        // the digger, and nothing is lost (charter rule 5).
        bot.dialog_event(FORM, DialogEvent::Closed)
            .await
            .expect("send");
        bot.sleep_ticks(4).await;
        bot.select_tool(None).await.expect("send");
        bot.start_dig(centre_of(chest_at())).await.expect("send");
        assert!(
            until(&mut bot, |bot| bot.block_is_empty(chest_at())).await,
            "the chest was not dug; notices {:?}",
            bot.notices()
        );
        assert!(
            until(&mut bot, |bot| bot.units_of(chest) == 54).await,
            "the block and its contents did not both come back; inventory {:?}",
            bot.inventory()
        );
    });
    assert!(server.stop());
}

#[test]
fn a_chest_somebody_else_has_open_refuses_the_dig_and_says_so() {
    let server = start("in-use");
    block_on(async {
        let mut keeper = join(&server, "Keeper").await;
        let chest = material_id(&keeper, "core_chest:chest");
        place_and_open(&mut keeper, chest).await;

        // Somebody else digs it while it is open: refused, in words, and the
        // chest stays where it is.
        // A block that was there before the thief joined arrives in the chunk,
        // not as an edit, so there is no delta to wait for: a few ticks to
        // settle is what a joiner gets.
        let mut thief = join(&server, "Thief").await;
        thief.sleep_ticks(10).await;
        thief.select_tool(None).await.expect("send");
        thief.start_dig(centre_of(chest_at())).await.expect("send");
        assert!(
            until(&mut thief, |bot| told(bot, "somebody is using that")).await,
            "the dig of an open chest was not refused; notices {:?}",
            thief.notices()
        );
        assert!(
            !thief.block_is_empty(chest_at()),
            "the chest was dug out from under the player holding it"
        );

        // The keeper shuts it, and the next dig goes through.
        keeper
            .dialog_event(FORM, DialogEvent::Closed)
            .await
            .expect("send");
        keeper.sleep_ticks(4).await;
        thief.stop_dig().await.expect("send");
        thief.start_dig(centre_of(chest_at())).await.expect("send");
        assert!(
            until(&mut thief, |bot| bot.block_is_empty(chest_at())).await,
            "a chest nobody has open could not be dug; notices {:?}",
            thief.notices()
        );
    });
    assert!(server.stop());
}
