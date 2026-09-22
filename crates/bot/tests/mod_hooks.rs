// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Cancellable mod hooks, over a real server.
//!
//! Charter rule 1: the mod API is the only API, and rules about what a player
//! may do belong in mods rather than in the engine. `on_dig_complete` and
//! `on_place` are how a mod says no — a protection plugin, a claim system, a
//! tutorial that will not let you break the wrong thing.
//!
//! # Every veto test is paired with a permissive twin
//!
//! "The block is still there" is satisfied by a server where digging is broken
//! for reasons that have nothing to do with the hook. So each refusal test has
//! a counterpart running the SAME scenario against a mod whose hook returns
//! nothing, and asserts the action goes through. A change that broke digging
//! outright would pass the first test and fail the second.

use std::path::{Path, PathBuf};
use std::time::Duration;

use bot::Bot;
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::interest::ViewDistance;
use tiamat_core::{BlockPos, MaterialId, SubNodePos};
use tiamat_server::{ServerHandle, Settings};

const MATERIALS: [&str; 1] = ["test:stone"];

fn stone() -> u16 {
    let mut registry = tiamat_core::Registry::new();
    let mut id = MaterialId::AIR;
    for name in MATERIALS {
        id = registry.register(name).expect("register");
    }
    id.0
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamat-mod-hooks").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Writes a mod directory holding a default tool and the given hook bodies.
///
/// The tool registration is not incidental: the engine has no bare hand of its
/// own (charter rule 1), so a mod set with no tools is one nobody can dig in —
/// and a veto test against a world where digging was impossible anyway would
/// prove nothing at all.
fn write_warden(name: &str, hooks: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("warden");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"warden\"\nname = \"Warden\"\nversion = \"0.1.0\"\n\
         license = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        format!(
            // Ground, as well as a tool. **An empty world is not a neutral
            // fixture**: with nothing to stand on the player free-falls, their
            // eye leaves the block under test, and the server refuses the dig
            // for being out of reach — which looks exactly like the hook
            // vetoing it. The reference generator does the same thing; this is
            // the smallest version of it.
            "local ground = game.register_block{{ id = \"ground\" }}\n\
             game.register_on_generate(function(buf, pos)\n\
             \x20   buf:fill_below_heightmap(game.flat_heightmap(0), ground)\n\
             end)\n\
             game.register_tool{{\n\
             \x20   id = \"hand\",\n\
             \x20   brush = \"block\",\n\
             \x20   speed_multiplier = 1.0,\n\
             \x20   default = true,\n\
             }}\n\
             {hooks}\n"
        ),
    )
    .expect("script");
    root
}

/// A mod that registers no tools at all, for the empty-table case.
fn write_warden_without_tools(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("quiet");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"quiet\"\nname = \"Quiet\"\nversion = \"0.1.0\"\n\
         license = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(dir.join("init.lua"), "-- registers nothing\n").expect("script");
    root
}

fn start(name: &str, mods: PathBuf) -> ServerHandle {
    ServerHandle::start(&Settings {
        world_options: Vec::new(),
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: scratch(&format!("{name}-world")),
        identity_path: None,
        max_players: 4,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(mods.clone()),
        enabled_mods: bot::fixture::enabled_mods_for(&mods).expect("the reference mods' manifests"),
        seed: Some(3),
        rcon: None,
        materials: MATERIALS.iter().map(|name| (*name).to_owned()).collect(),
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

async fn join(server: &ServerHandle) -> Bot {
    let mut bot = Bot::connect(
        server.local_addr(),
        Identity::generate().expect("identity"),
        server.cert_fingerprint(),
    )
    .await
    .expect("connect");
    bot.join("Subject").await.expect("join");
    bot
}

fn centre_of(pos: BlockPos) -> SubNodePos {
    SubNodePos::new(pos.x * 3 + 1, pos.y * 3 + 1, pos.z * 3 + 1)
}

/// Seeds a block, digs it, and reports whether the server removed it.
///
/// The Task 07 direct-edit path seeds it, because what is under test is the
/// dig, not the seeding.
async fn dig_and_see(bot: &mut Bot, server: &ServerHandle, pos: BlockPos, material: u16) -> bool {
    // Seeded by the OPERATOR, not by the bot. A client cannot edit the world,
    // and a test that arranged one by pretending otherwise would be asserting
    // against a capability that should not exist.
    assert!(server.seed_block(pos, material), "seed queue full");
    bot.expect_block(pos, material, Duration::from_secs(10))
        .await
        .expect("the seed should land");

    bot.select_tool(None).await.expect("bare hand");
    bot.start_dig(centre_of(pos)).await.expect("start dig");

    // A whole block at the default hardness is 15 ticks; three seconds is
    // several times that, so a timeout here means it is not going to happen
    // rather than that it has not happened yet.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        let _ = tokio::time::timeout(Duration::from_millis(200), bot.recv()).await;
        // A block comes apart a sub-node at a time now, so "was it removed"
        // counts the pieces — digging never sends a whole-block edit. A mod's
        // `set_block` still can, which `block_is_empty` also honours.
        if bot.block_is_empty(pos) {
            return true;
        }
    }
    false
}

#[test]
fn a_mod_can_refuse_a_dig() {
    let server = start(
        "dig-refused",
        write_warden(
            "dig-refused",
            "game.register_on_dig_complete(function() return false end)",
        ),
    );
    let stone = stone();

    block_on(async {
        let mut bot = join(&server).await;
        let removed = dig_and_see(&mut bot, &server, BlockPos::new(2, -1, 0), stone).await;
        assert!(
            !removed,
            "the mod refused the dig and the block went anyway"
        );
        assert!(
            bot.inventory().is_empty(),
            "a refused dig still credited the player: {:?}",
            bot.inventory()
        );
    });

    assert!(server.stop());
}

#[test]
fn a_hook_that_does_not_refuse_lets_the_dig_through() {
    // The twin of the test above. Without this, "the block survived" would also
    // be satisfied by a server on which nothing can be dug at all.
    let server = start(
        "dig-allowed",
        write_warden(
            "dig-allowed",
            "seen = 0\ngame.register_on_dig_complete(function() seen = seen + 1 end)",
        ),
    );
    let stone = stone();

    block_on(async {
        let mut bot = join(&server).await;
        let removed = dig_and_see(&mut bot, &server, BlockPos::new(2, -1, 0), stone).await;
        assert!(
            removed,
            "a hook that returned nothing cancelled the dig; only an explicit false should"
        );
    });

    assert!(server.stop());
}

#[test]
fn a_mod_that_throws_while_vetoing_does_not_stop_the_dig() {
    // Charter rule 10 through the whole stack: a crash disables that mod and
    // the world keeps working. If a fault counted as a refusal, one broken mod
    // would make the server unmineable for everybody.
    let server = start(
        "dig-throws",
        write_warden(
            "dig-throws",
            "game.register_on_dig_complete(function() error('boom') end)",
        ),
    );
    let stone = stone();

    block_on(async {
        let mut bot = join(&server).await;
        let removed = dig_and_see(&mut bot, &server, BlockPos::new(2, -1, 0), stone).await;
        assert!(
            removed,
            "a mod's crash was treated as a veto, so one bad mod can stop the server digging"
        );
    });

    assert!(server.stop());
}

/// The inventory, once it has stopped changing.
///
/// **Sampling after the first update is a bet on how fast the machine is.** A
/// dig credits as it breaks, so the first `InventoryUpdate` to arrive is not
/// necessarily the last, and a reading taken there differs from one taken a
/// moment later — which a test comparing before with after then reports as the
/// server having charged somebody. That is what went red on Windows CI, and on
/// macOS before it, in `a_mod_can_refuse_a_placement_and_the_player_keeps_their_material`.
///
/// Waits for a quiet beat rather than a fixed sleep: quiet is the condition
/// that actually matters, and a sleep long enough for a loaded runner is dead
/// time on every other one.
async fn settled_inventory(bot: &mut Bot) -> Vec<tiamat_core::proto::StackDef> {
    fn updates(bot: &Bot) -> usize {
        bot.received()
            .iter()
            .filter(|message| {
                matches!(
                    message,
                    tiamat_core::proto::ServerMessage::InventoryUpdate { .. }
                )
            })
            .count()
    }

    let mut seen = updates(bot);
    for _ in 0..25 {
        bot.sleep_ticks(4).await;
        let now = updates(bot);
        if now == seen {
            break;
        }
        seen = now;
    }
    bot.inventory()
}

#[test]
fn a_mod_can_refuse_a_placement_and_the_player_keeps_their_material() {
    let server = start(
        "place-refused",
        write_warden(
            "place-refused",
            "game.register_on_place(function() return false end)",
        ),
    );
    let stone = stone();

    block_on(async {
        let mut bot = join(&server).await;

        // Mine a block so there is something to place, which also proves the
        // veto is specific to placing rather than a mod that broke everything.
        let quarry = BlockPos::new(2, -1, 0);
        assert!(
            dig_and_see(&mut bot, &server, quarry, stone).await,
            "digging should still work; only placing is refused here"
        );
        bot.await_inventory(Duration::from_secs(10))
            .await
            .expect("the dig should credit");
        let before = settled_inventory(&mut bot).await;
        assert!(!before.is_empty(), "nothing was credited to place with");

        let target = BlockPos::new(-2, 0, 0);
        bot.place_from_inventory(centre_of(target), stone)
            .await
            .expect("send");
        tokio::time::sleep(Duration::from_millis(500)).await;
        let _ = tokio::time::timeout(Duration::from_millis(500), bot.recv()).await;

        assert!(
            bot.notices()
                .iter()
                .any(|text| text.contains("cannot build")),
            "the refusal was silent; notices {:?}",
            bot.notices()
        );
        assert_eq!(
            settled_inventory(&mut bot).await,
            before,
            "a refused placement charged the player anyway"
        );
    });

    assert!(server.stop());
}

#[test]
fn a_hook_can_refuse_selectively_using_the_event_it_is_given() {
    // The useful case, and the one that proves the event payload is real: a
    // mod that protects one region and allows everywhere else. A hook that
    // could only say "no to everything" would not be worth having.
    let guarded = BlockPos::new(2, -1, 0);
    let server = start(
        "selective",
        write_warden(
            "selective",
            &format!(
                "game.register_on_dig_complete(function(e)\n\
                 \x20   if e.x >= {} and e.x <= {} then return false end\n\
                 end)",
                guarded.x * 3,
                guarded.x * 3 + 2,
            ),
        ),
    );
    let stone = stone();

    block_on(async {
        let mut bot = join(&server).await;
        assert!(
            !dig_and_see(&mut bot, &server, guarded, stone).await,
            "the guarded block was dug"
        );
        assert!(
            dig_and_see(&mut bot, &server, BlockPos::new(-2, -1, 0), stone).await,
            "a block outside the guarded range was refused too, so the hook is not reading \
             the event"
        );
    });

    assert!(server.stop());
}

#[test]
fn a_veto_hook_can_read_the_world_it_is_judging() {
    // Reported by the world mod: a bush with blooms is picked by cancelling
    // its dig, and the hook has to LOOK at the bush to know whether there is
    // anything to pick. `game.get_block` answered nil there for the very
    // block being dug, with the player standing next to it, because the dig
    // path asked the mods outside the window the world is lent in.
    //
    // Each hook refuses with what it read, so the notice carries the answer
    // out. "blind" is the bug; the other two words say it looked and saw the
    // right thing, which a hook that merely got a table back would not.
    let server = start(
        "veto-reads",
        write_warden(
            "veto-reads",
            "game.register_on_dig_complete(function(e)\n\
             \x20   local at = game.get_block{ x = e.x // 3, y = e.y // 3, z = e.z // 3 }\n\
             \x20   if at == nil then return 'dig blind' end\n\
             \x20   if e.x // 3 ~= 2 then return end\n\
             \x20   if at.material == e.material then return 'dig saw the block' end\n\
             \x20   return 'dig saw something else'\n\
             end)\n\
             game.register_on_place(function(e)\n\
             \x20   local at = game.get_block{ x = e.x, y = e.y, z = e.z }\n\
             \x20   if at == nil then return 'place blind' end\n\
             \x20   if at.material == game.AIR then return 'place saw air' end\n\
             \x20   return 'place saw something else'\n\
             end)",
        ),
    );
    let stone = stone();

    block_on(async {
        let mut bot = join(&server).await;
        let pos = BlockPos::new(2, -1, 0);
        assert!(
            !dig_and_see(&mut bot, &server, pos, stone).await,
            "the hook refuses every dig, so the block should have stayed"
        );
        let told = |bot: &Bot, word: &str| bot.notices().iter().any(|text| text.starts_with(word));
        assert!(
            told(&bot, "dig saw the block"),
            "the dig hook could not read the block it was judging; notices {:?}",
            bot.notices()
        );

        // Something to place with, the way a player gets it: the hook lets
        // every dig but the probe's through.
        assert!(
            dig_and_see(&mut bot, &server, BlockPos::new(-3, -1, 0), stone).await,
            "a dig away from the probe should be allowed"
        );
        assert!(
            !told(&bot, "dig blind"),
            "the second dig's hook could not read; notices {:?}",
            bot.notices()
        );
        bot.await_inventory(Duration::from_secs(10))
            .await
            .expect("the dig should credit");
        settled_inventory(&mut bot).await;
        bot.place_from_inventory(centre_of(BlockPos::new(-2, 0, 0)), stone)
            .await
            .expect("send");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline && !told(&bot, "place") {
            let _ = tokio::time::timeout(Duration::from_millis(200), bot.recv()).await;
        }
        assert!(
            told(&bot, "place saw air"),
            "the place hook could not read where it was asked about; notices {:?}",
            bot.notices()
        );
    });

    assert!(server.stop());
}

#[test]
fn a_use_of_a_block_reaches_the_mods_and_an_unhandled_one_gets_the_engines_word() {
    // Asked for by the world mod, to pick roses: right-click a bush with an
    // empty hand. A client used to answer that itself with a warning and send
    // nothing, so `register_on_use` is the first time a mod has heard it.
    //
    // The hook reads the block it is asked about (the world is lent) and
    // answers with what it saw and what was in the hand, so the notice carries
    // it out. A use it lets pass is answered with the warning the client used
    // to show, which is what keeps an unmodded world the same to play.
    let server = start(
        "use",
        write_warden(
            "use",
            "game.register_on_use(function(e)\n\
             \x20   if e.x // 3 ~= 2 then return end\n\
             \x20   local at = game.get_block{ x = e.x // 3, y = e.y // 3, z = e.z // 3 }\n\
             \x20   local looked = at and at.material == e.material\n\
             \x20   return 'picked: looked=' .. tostring(looked) .. ' held=' .. tostring(e.held)\n\
             end)",
        ),
    );
    let stone = stone();

    block_on(async {
        let mut bot = join(&server).await;
        let bush = BlockPos::new(2, -1, 0);
        assert!(server.seed_block(bush, stone), "seed queue full");
        bot.expect_block(bush, stone, Duration::from_secs(10))
            .await
            .expect("the seed should land");

        let told = |bot: &Bot, word: &str| bot.notices().iter().any(|text| text.starts_with(word));
        async fn wait_for(bot: &mut Bot, word: &str) {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            while tokio::time::Instant::now() < deadline
                && !bot.notices().iter().any(|text| text.starts_with(word))
            {
                let _ = tokio::time::timeout(Duration::from_millis(200), bot.recv()).await;
            }
        }

        bot.use_block(centre_of(bush)).await.expect("send");
        wait_for(&mut bot, "picked").await;
        assert!(
            told(&bot, "picked: looked=true held=nil"),
            "the mod did not handle the use, could not read the block, or saw a hand that \
             was not empty; notices {:?}",
            bot.notices()
        );
        assert!(
            !told(&bot, "nothing selected"),
            "a handled use was also answered with the engine's warning; notices {:?}",
            bot.notices()
        );

        // The ground beside it, which the hook lets pass.
        bot.use_block(centre_of(BlockPos::new(-2, -1, 0)))
            .await
            .expect("send");
        wait_for(&mut bot, "nothing selected").await;
        assert!(
            told(&bot, "nothing selected to build with"),
            "a use nobody handled said nothing; notices {:?}",
            bot.notices()
        );

        // And reach is the server's, as for a dig.
        bot.use_block(centre_of(BlockPos::new(2, -1, 40)))
            .await
            .expect("send");
        wait_for(&mut bot, "that is too far").await;
        assert!(
            told(&bot, "that is too far away"),
            "a use out of reach was not refused; notices {:?}",
            bot.notices()
        );
    });

    assert!(server.stop());
}

/// Where the reference mods live, for the test below.
fn reference_mods() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../game")
        .canonicalize()
        .expect("the reference mods live at the repo root")
}

#[test]
fn a_client_is_told_which_tools_the_mods_registered() {
    // Charter rule 1, from the client's side. The engine has no tools of its
    // own — not even a bare hand — so a client cannot offer a way to choose one
    // without being told what exists, and hard-coding `core_tools:chisel` into
    // the client would be exactly the special-casing rule 1 forbids.
    //
    // This is why protocol v7 exists. Before it, the chisel was registered,
    // reachable over the wire, and impossible to select from the window: there
    // was no way for the client to know its name.
    let server = start("tool-table", reference_mods());

    block_on(async {
        let bot = join(&server).await;
        let tools: Vec<tiamat_core::proto::ToolDef> = bot
            .received()
            .into_iter()
            .find_map(|message| match message {
                tiamat_core::proto::ServerMessage::ToolTable { tools } => Some(tools),
                _ => None,
            })
            .expect("the server should send a tool table on join");

        let chisel = tools
            .iter()
            .find(|tool| tool.id == "core_tools:chisel")
            .expect("the reference mods register a chisel");
        assert_eq!(
            chisel.brush, "subnode",
            "the chisel's brush is the whole reason sub-nodes exist"
        );
        assert_eq!(chisel.name, "Chisel", "the mod's display name was dropped");
        assert!(!chisel.default);

        // And exactly one default, or a client cannot tell what a bare hand is.
        let defaults: Vec<&str> = tools
            .iter()
            .filter(|tool| tool.default)
            .map(|tool| tool.id.as_str())
            .collect();
        assert_eq!(
            defaults,
            vec!["core_tools:hand"],
            "expected exactly one default tool"
        );
    });

    assert!(server.stop());
}

#[test]
fn a_world_with_no_tool_mods_sends_an_empty_table() {
    // The counter-example, and the shape charter rule 1 actually asks for: an
    // engine with no mods has no tools, so the table is empty and nothing in
    // that world can be dug. Correct rather than broken.
    let server = start("no-tools", write_warden_without_tools("no-tools"));

    block_on(async {
        let bot = join(&server).await;
        let tools = bot
            .received()
            .into_iter()
            .find_map(|message| match message {
                tiamat_core::proto::ServerMessage::ToolTable { tools } => Some(tools),
                _ => None,
            })
            .expect("a tool table should be sent even when it is empty");
        assert!(
            tools.is_empty(),
            "a mod set that registers no tools produced {tools:?}"
        );
    });

    assert!(server.stop());
}

#[test]
fn the_reference_mods_register_no_vetoes() {
    // `game/` is a set of test fixtures, not a game (charter scope discipline),
    // and nothing in it should be quietly refusing player actions. If this ever
    // fails, a fixture has grown an opinion — which would make every other
    // test in the suite depend on it.
    let server = start("reference", reference_mods());
    let stone = stone();

    block_on(async {
        let mut bot = join(&server).await;
        assert!(
            dig_and_see(&mut bot, &server, BlockPos::new(2, -1, 0), stone).await,
            "a reference mod is refusing digs"
        );
    });

    assert!(server.stop());
}
