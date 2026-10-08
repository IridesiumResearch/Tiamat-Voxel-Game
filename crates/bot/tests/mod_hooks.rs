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

/// Two mods for the tag-speed test (Craft ask 11).
///
/// `smithy` registers a pick whose only per-material speed is `"#hard"`,
/// and loads FIRST — topological order ties break alphabetically, and
/// `smithy` sorts before `world` — so the block that
/// speed reaches is one `world` registers after `smithy` has already
/// finished loading. If the speed only resolved when `register_tool` ran,
/// it would find nothing to attach to and this test's fast dig would not be
/// fast. `world` registers the ground, one `hard`-tagged block and one
/// plain block of the SAME hardness, so a difference in dig time between
/// them cannot be explained by anything but the tag.
fn write_tag_speed_mods(name: &str) -> PathBuf {
    let root = scratch(name);

    let smithy = root.join("smithy");
    std::fs::create_dir_all(&smithy).expect("mod dir");
    std::fs::write(
        smithy.join("mod.toml"),
        "id = \"smithy\"\nname = \"Smithy\"\nversion = \"0.1.0\"\n\
         license = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        smithy.join("init.lua"),
        "game.register_tool{\n\
         \x20   id = \"pick\",\n\
         \x20   brush = \"block\",\n\
         \x20   speed_multiplier = 1.0,\n\
         \x20   default = true,\n\
         \x20   speeds = { [\"#hard\"] = 6.0 },\n\
         }\n",
    )
    .expect("script");

    let world = root.join("world");
    std::fs::create_dir_all(&world).expect("mod dir");
    std::fs::write(
        world.join("mod.toml"),
        "id = \"world\"\nname = \"World\"\nversion = \"0.1.0\"\n\
         license = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        world.join("init.lua"),
        "local ground = game.register_block{ id = \"ground\" }\n\
         game.register_block{ id = \"stone_hard\", hardness = 3.0, tags = { \"hard\" } }\n\
         game.register_block{ id = \"stone_plain\", hardness = 3.0 }\n\
         game.register_on_generate(function(buf, pos)\n\
         \x20   buf:fill_below_heightmap(game.flat_heightmap(0), ground)\n\
         end)\n",
    )
    .expect("script");

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

/// Seeds a block, digs it with whatever tool is already selected, and
/// reports how many ticks the removal took — or `None` if it never came off.
///
/// The Craft ask 11 tag-speed test needs a measure of how long the dig took
/// that a difference in tool speed shows up in and a loaded CI runner cannot
/// swamp. Wall-clock elapsed time is the wrong tool for that: the seed round
/// trip, the `StartDig` network hop and this loop's own polling all add a
/// fixed overhead to EVERY dig regardless of how many ticks the server
/// actually spent on it, so a ratio like "half the time" is really a ratio
/// of `ticks * tick_length + overhead`, and a slow runner inflating
/// `overhead` can push that ratio outside the assertion even though the
/// feature works exactly as intended (a real flake class here — see the
/// `settled_inventory` doc comment above for another one). Ticks side-step
/// it: the server emits one `DigProgress` for the target per tick a dig is
/// running (`endpoint.rs`, alongside `PlayerState`, every tick), so counting
/// them measures server-side work directly, independent of how long the
/// client took to notice any of it.
async fn dig_and_count_ticks(
    bot: &mut Bot,
    server: &ServerHandle,
    pos: BlockPos,
    material: u16,
    patience: Duration,
) -> Option<usize> {
    assert!(server.seed_block(pos, material), "seed queue full");
    bot.expect_block(pos, material, Duration::from_secs(10))
        .await
        .expect("the seed should land");

    let target = centre_of(pos);
    let since = bot.received().len();
    bot.start_dig(target).await.expect("start dig");

    let deadline = tokio::time::Instant::now() + patience;
    while tokio::time::Instant::now() < deadline {
        let _ = tokio::time::timeout(Duration::from_millis(50), bot.recv()).await;
        if bot.block_is_empty(pos) {
            let ticks = bot
                .received()
                .iter()
                .skip(since)
                .filter(|message| {
                    matches!(
                        message,
                        tiamat_core::proto::ServerMessage::DigProgress { target: t, .. }
                            if *t == target
                    )
                })
                .count();
            return Some(ticks);
        }
    }
    None
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
fn a_mod_can_refuse_a_dig_before_it_starts_and_the_player_hears_why_at_once() {
    // Craft ask 1. `on_dig_complete` is asked at the first chip, which for a
    // one-cell dig is after the whole wait; a tool gate wants to say "not
    // with that" as the player starts. The block stays, nothing is credited,
    // and the reason reaches the player.
    let server = start(
        "dig-gated",
        write_warden(
            "dig-gated",
            "game.register_on_dig_start(function() return 'that needs a pick' end)",
        ),
    );
    let stone = stone();

    block_on(async {
        let mut bot = join(&server).await;
        let removed = dig_and_see(&mut bot, &server, BlockPos::new(2, -1, 0), stone).await;
        assert!(
            !removed,
            "the gate refused the dig and the block went anyway"
        );
        assert!(
            bot.inventory().is_empty(),
            "a dig refused at its start still credited the player: {:?}",
            bot.inventory()
        );
        assert!(
            bot.notices()
                .iter()
                .any(|text| text.starts_with("that needs a pick")),
            "the player was not told why; notices {:?}",
            bot.notices()
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

#[test]
fn a_mod_can_read_one_slot_of_a_players_view() {
    // Craft ask 9: the off-hand, for a station worked in the world. The
    // warden gives into slot 3 and reads it back; slot 28 is empty and says
    // so; it marks the world only if both are as expected.
    let server = start(
        "slot-read",
        write_warden(
            "slot-read",
            "game.register_block{ id = 'mark' }\n\
             game.register_on_player_join(function(e)\n\
             \x20   assert(game.give(e.player, { material = 'warden:ground', units = 5, slot = 3 }))\n\
             \x20   local got = game.slot(e.player, 'player:main', 3)\n\
             \x20   local none = game.slot(e.player, 'player:main', 28)\n\
             \x20   local ground = game.get_block_id('warden:ground')\n\
             \x20   if got and got.units == 5 and got.material == ground and none == nil then\n\
             \x20       game.set_block({ x = 9, y = 10, z = 1 }, 'warden:mark')\n\
             \x20   end\n\
             end)",
        ),
    );

    block_on(async {
        let mut bot = join(&server).await;
        let mark = material_named(&bot, "warden:mark");
        bot.expect_block(BlockPos::new(9, 10, 1), mark, Duration::from_secs(10))
            .await
            .expect("the slot read back what was given into it");
    });

    assert!(server.stop());
}

#[test]
fn a_mod_hears_a_player_cross_into_another_block() {
    // Progress ask 3. The first time, the tick after the join, has no
    // `from`; a walk after that has one.
    let server = start(
        "moves",
        write_warden(
            "moves",
            "game.register_block{ id = 'mark' }\n\
             game.register_on_player_move(function(e)\n\
             \x20   if e.from == nil then\n\
             \x20       game.set_block({ x = 9, y = 10, z = 1 }, 'warden:mark')\n\
             \x20   else\n\
             \x20       game.set_block({ x = 11, y = 10, z = 1 }, 'warden:mark')\n\
             \x20   end\n\
             end)",
        ),
    );

    block_on(async {
        let mut bot = join(&server).await;
        let mark = material_named(&bot, "warden:mark");
        bot.expect_block(BlockPos::new(9, 10, 1), mark, Duration::from_secs(10))
            .await
            .expect("the first block a player is placed in is heard");
        bot.walk([1.0, 0.0, 0.0], 0, 40).await.expect("walk");
        bot.expect_block(BlockPos::new(11, 10, 1), mark, Duration::from_secs(10))
            .await
            .expect("a walk into the next block is heard, with where they came from");
    });

    assert!(server.stop());
}

#[test]
fn a_fixed_main_view_refuses_what_does_not_fit_and_says_how_much() {
    // UI ask 14. The warden fixes the main view at one slot, which holds
    // ninety blocks (2430 units), and gives 2436 on join: the slot fills,
    // `give` answers false and six,
    // and the view has not grown. It marks the world only when all of that
    // held.
    let server = start(
        "main-fixed",
        write_warden(
            "main-fixed",
            "game.set_main_slots(1)\n\
             game.register_block{ id = 'mark' }\n\
             game.register_on_player_join(function(e)\n\
             \x20   local gave, left = game.give(e.player, { material = 'warden:ground', units = 2436 })\n\
             \x20   local slots = 0\n\
             \x20   for n = 1, 4 do if game.slot(e.player, 'player:main', n) then slots = slots + 1 end end\n\
             \x20   if gave == false and left == 6 and slots == 1 and game.slot(e.player, 'player:main', 2) == nil then\n\
             \x20       game.set_block({ x = 9, y = 10, z = 1 }, 'warden:mark')\n\
             \x20   end\n\
             end)",
        ),
    );

    block_on(async {
        let mut bot = join(&server).await;
        let mark = material_named(&bot, "warden:mark");
        bot.expect_block(BlockPos::new(9, 10, 1), mark, Duration::from_secs(10))
            .await
            .expect("the give was refused past the second slot, and said so");
        let stacks = settled_inventory(&mut bot).await;
        let ground = material_named(&bot, "warden:ground");
        assert_eq!(
            units_of(&stacks, ground),
            2430,
            "one slot, full: {stacks:?}"
        );
    });

    assert!(server.stop());
}

#[test]
fn a_full_fixed_inventory_stops_a_dig_before_the_block_comes_apart() {
    // UI ask 14's other half: with nowhere to put the yield, the bite is
    // not taken. The block stays whole, nothing is credited past what fit,
    // and the player is told.
    let server = start(
        "hands-full",
        write_warden(
            "hands-full",
            "game.set_main_slots(1)\n\
             game.register_on_player_join(function(e)\n\
             \x20   game.give(e.player, { material = 'warden:ground', units = 2430 })\n\
             end)",
        ),
    );
    let stone = stone();

    block_on(async {
        let mut bot = join(&server).await;
        bot.sleep_ticks(4).await;
        let removed = dig_and_see(&mut bot, &server, BlockPos::new(2, -1, 0), stone).await;
        assert!(
            !removed,
            "a dig with nowhere to put the yield took the block apart"
        );
        assert!(
            bot.notices()
                .iter()
                .any(|text| text.starts_with("you cannot carry any more")),
            "the player was not told; notices {:?}",
            bot.notices()
        );
        let stacks = settled_inventory(&mut bot).await;
        let ground = material_named(&bot, "warden:ground");
        assert_eq!(
            units_of(&stacks, ground),
            2430,
            "the one slot, as given: {stacks:?}"
        );
        assert_eq!(
            stacks.iter().map(|stack| stack.units).sum::<u32>(),
            2430,
            "something was credited past the one slot: {stacks:?}"
        );
    });

    assert!(server.stop());
}

/// The world id the server gave a mod's block, from the table sent on join.
fn material_named(bot: &Bot, name: &str) -> u16 {
    bot.material_table()
        .expect("the material table arrives on join")
        .iter()
        .find(|def| def.name == name)
        .map(|def| def.id)
        .unwrap_or_else(|| panic!("no material `{name}` in the table"))
}

fn units_of(stacks: &[tiamat_core::proto::StackDef], material: u16) -> u32 {
    stacks
        .iter()
        .filter(|stack| stack.material == material)
        .map(|stack| stack.units)
        .sum()
}

#[test]
fn a_mod_can_say_what_a_dig_yields() {
    // Craft ask 3: rubble by hand, ore by pick. The hook's answer replaces
    // the block's own rule for this dig, in units per full block, paid as the
    // block comes apart — so a whole block at two a cell is exactly 54, and
    // none of the stone that was actually there.
    let server = start(
        "dig-yields",
        write_warden(
            "dig-yields",
            "game.register_block{ id = 'gem' }\n\
             game.register_on_dig_complete(function(e)\n\
             \x20   return { drops = { gem = 54 } }\n\
             end)",
        ),
    );

    block_on(async {
        let mut bot = join(&server).await;
        // **World ids, from the table.** What a chunk holds and what an
        // inventory carries are world ids, and `stone()` is the runtime id
        // — the two number the same materials differently.
        let stone = material_named(&bot, "test:stone");
        let gem = material_named(&bot, "warden:gem");
        let removed = dig_and_see(&mut bot, &server, BlockPos::new(2, -1, 0), stone).await;
        assert!(removed, "an answer with drops was taken for a refusal");
        let stacks = settled_inventory(&mut bot).await;
        assert_eq!(
            units_of(&stacks, gem),
            54,
            "the hook's drops were not paid: {stacks:?}"
        );
        assert_eq!(
            units_of(&stacks, stone),
            0,
            "the block itself was paid as well: {stacks:?}"
        );
    });

    assert!(server.stop());
}

#[test]
fn a_blocks_registered_drops_apply_and_may_name_another_mods_block() {
    // Two things at once. `register_block{ drops = ... }` was accepted and
    // never consulted: a dig credited what the edit removed regardless. And
    // a drop naming another mod's block was refused as if it registered one
    // (Craft ask 7) — here the ore drops the `test:` namespace's stone,
    // which is not the warden's, and three of its own gems a block.
    let server = start(
        "dig-drops",
        write_warden(
            "dig-drops",
            "game.register_block{ id = 'ore', hardness = 0.5, drops = { ['test:stone'] = 27, gem = 3 } }\n\
             game.register_block{ id = 'gem' }",
        ),
    );

    block_on(async {
        let mut bot = join(&server).await;
        let stone = material_named(&bot, "test:stone");
        let ore = material_named(&bot, "warden:ore");
        let gem = material_named(&bot, "warden:gem");
        let removed = dig_and_see(&mut bot, &server, BlockPos::new(2, -1, 0), ore).await;
        assert!(removed, "the ore did not come apart");
        let stacks = settled_inventory(&mut bot).await;
        assert_eq!(
            units_of(&stacks, stone),
            27,
            "the stone it drops was not paid: {stacks:?}"
        );
        assert_eq!(
            units_of(&stacks, gem),
            3,
            "three gems a block, exactly: {stacks:?}"
        );
        assert_eq!(
            units_of(&stacks, ore),
            0,
            "the ore itself was paid as well: {stacks:?}"
        );
    });

    assert!(server.stop());
}

#[test]
fn a_tag_speed_digs_a_later_registered_tagged_block_faster() {
    // Craft ask 11(b): `register_tool{ speeds = { ["#hard"] = 6.0 } }`
    // resolves at freeze, once every mod has registered — not when
    // `register_tool` ran — so it still reaches `world:stone_hard`, a block
    // named only by `world`, a mod that loads AFTER `smithy`. Comparing
    // against `world:stone_plain`, the SAME hardness but no tag, is what
    // rules out "the tool just digs everything faster than usual": nothing
    // differs between the two digs but the tag.
    let server = start("tag-speed", write_tag_speed_mods("tag-speed"));

    block_on(async {
        let mut bot = join(&server).await;
        let hard = material_named(&bot, "world:stone_hard");
        let plain = material_named(&bot, "world:stone_plain");

        bot.select_tool(None).await.expect("the default pick");

        // A wall-clock ceiling only — generous enough that CI jitter alone
        // cannot time either dig out. The comparison below is on ticks, not
        // this, so a slow runner cannot make it flaky (`dig_and_count_ticks`
        // explains why).
        let patience = Duration::from_secs(8);
        let tagged =
            dig_and_count_ticks(&mut bot, &server, BlockPos::new(2, -1, 0), hard, patience)
                .await
                .expect("the tag-sped block should come apart");
        let untagged =
            dig_and_count_ticks(&mut bot, &server, BlockPos::new(-2, -1, 0), plain, patience)
                .await
                .expect("the plain block of the same hardness should come apart too");

        assert!(
            tagged * 2 < untagged,
            "a #hard speed of 6x on hardness 3.0 should dig in roughly a \
             sixth of the tick count, not something comparable: tagged \
             {tagged} ticks, untagged {untagged} ticks"
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

#[test]
fn a_use_at_nothing_reaches_a_mod_that_asked_for_one_and_is_answered_like_any_other() {
    // Protocol v76, for the Life mod's meals: the place control at open sky
    // with food in hand. A mod that registered `anywhere` hears a use with no
    // cell and the hand as ever; a use it lets pass gets the engine's word,
    // exactly as one at a block does.
    let server = start(
        "use-at-nothing",
        write_warden(
            "use-at-nothing",
            "heard = 0\n\
             game.register_on_use(function(e)\n\
             \x20   heard = heard + 1\n\
             \x20   if heard > 1 then return end\n\
             \x20   return 'at nothing: x=' .. tostring(e.x) .. ' held=' .. tostring(e.held)\n\
             end, { anywhere = true })",
        ),
    );

    block_on(async {
        let mut bot = join(&server).await;
        let told = |bot: &Bot, word: &str| bot.notices().iter().any(|text| text.starts_with(word));
        async fn wait_for(bot: &mut Bot, word: &str) {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            while tokio::time::Instant::now() < deadline
                && !bot.notices().iter().any(|text| text.starts_with(word))
            {
                let _ = tokio::time::timeout(Duration::from_millis(200), bot.recv()).await;
            }
        }

        bot.use_at_nothing().await.expect("send");
        wait_for(&mut bot, "at nothing").await;
        assert!(
            told(&bot, "at nothing: x=nil held=nil"),
            "the mod did not hear a use at nothing, or it came with a cell; notices {:?}",
            bot.notices()
        );

        // The second one it lets pass: the warning an empty hand has always had.
        bot.use_at_nothing().await.expect("send");
        wait_for(&mut bot, "nothing selected").await;
        assert!(
            told(&bot, "nothing selected to build with"),
            "a use at nothing that nobody handled said nothing; notices {:?}",
            bot.notices()
        );
    });

    assert!(server.stop());
}

/// A `life`-like mod that eats whatever is held, anywhere, and a `craft`-like
/// mod with both of `on_use`'s slots filled: a listed handler for its own
/// fire, and an unlisted one for every station it cannot name (Craft ask 10).
///
/// `craft` names `life` in `optional_depends`, exactly as the real mods do
/// (ask 8's landing note), so `life` loads first without leaning on the
/// alphabetical tiebreak — the scenario the two slots exist for.
fn write_use_slots_fixture(name: &str) -> PathBuf {
    let root = scratch(name);

    let life = root.join("life");
    std::fs::create_dir_all(&life).expect("life dir");
    std::fs::write(
        life.join("mod.toml"),
        "id = \"life\"\nname = \"Life\"\nversion = \"0.1.0\"\n\
         license = \"GPL-3.0-only\"\n",
    )
    .expect("life manifest");
    std::fs::write(
        life.join("init.lua"),
        // Eats whatever is held, at any block or at nothing — the bot in
        // this test never holds anything, so `'ate'` never actually answers.
        // At the campfire specifically, an empty hand answers `'watched'`
        // instead of declining outright: Life loads before Craft and sits on
        // the same unlisted list Craft's own unlisted handler does, so if
        // ordering ever let an unlisted handler run ahead of a listed one,
        // Life would answer here before Craft's listed handler got the
        // chance to, and `'cooked'` below would never arrive — proving the
        // listed slot really is asked first, not just that both slots are
        // reachable. `game.get_block_id` is called from inside the callback,
        // not at the top of this file: Craft has not registered its blocks
        // yet when Life's init.lua runs (Craft depends on Life, so Life
        // loads first), only by the time a use reaches the callback, after
        // every mod has.
        "game.register_on_use(function(e)\n\
         \x20   if e.held then return 'ate' end\n\
         \x20   if e.material == game.get_block_id('craft:campfire') then return 'watched' end\n\
         end, { anywhere = true })\n",
    )
    .expect("life script");

    let craft = root.join("craft");
    std::fs::create_dir_all(&craft).expect("craft dir");
    std::fs::write(
        craft.join("mod.toml"),
        "id = \"craft\"\nname = \"Craft\"\nversion = \"0.1.0\"\n\
         license = \"GPL-3.0-only\"\noptional_depends = [\"life\"]\n",
    )
    .expect("craft manifest");
    std::fs::write(
        craft.join("init.lua"),
        "local ground = game.register_block{ id = \"ground\" }\n\
         game.register_on_generate(function(buf, pos)\n\
         \x20   buf:fill_below_heightmap(game.flat_heightmap(0), ground)\n\
         end)\n\
         game.register_tool{\n\
         \x20   id = \"hand\",\n\
         \x20   brush = \"block\",\n\
         \x20   speed_multiplier = 1.0,\n\
         \x20   default = true,\n\
         }\n\
         game.register_block{ id = \"campfire\" }\n\
         game.register_block{ id = \"anvil\" }\n\
         -- Listed: Craft's own fire, asked before anything unlisted.\n\
         game.register_on_use(function(e) return \"cooked\" end, \
           { materials = { \"campfire\" } })\n\
         -- Unlisted: heard at every OTHER block, in Craft's ordinary\n\
         -- load-order place — standing in for a station some other mod\n\
         -- adds after Craft loads, which Craft cannot name.\n\
         game.register_on_use(function(e) return \"stationed\" end)\n",
    )
    .expect("craft script");

    root
}

#[test]
fn a_use_reaches_a_mods_listed_handler_first_and_its_unlisted_one_elsewhere() {
    // Craft ask 10, end to end: `on_use`'s two slots, reached over a real
    // connection through the real dispatch path — not just the in-process
    // one the `mlua_vm` unit tests exercise.
    let server = start("use-slots", write_use_slots_fixture("use-slots"));

    block_on(async {
        let mut bot = join(&server).await;
        let campfire = material_named(&bot, "craft:campfire");
        let anvil = material_named(&bot, "craft:anvil");

        let fire_pos = BlockPos::new(2, -1, 0);
        let anvil_pos = BlockPos::new(-2, -1, 0);
        assert!(server.seed_block(fire_pos, campfire), "seed queue full");
        bot.expect_block(fire_pos, campfire, Duration::from_secs(10))
            .await
            .expect("the campfire should land");
        assert!(server.seed_block(anvil_pos, anvil), "seed queue full");
        bot.expect_block(anvil_pos, anvil, Duration::from_secs(10))
            .await
            .expect("the anvil should land");

        let told = |bot: &Bot, word: &str| bot.notices().iter().any(|text| text.starts_with(word));
        async fn wait_for(bot: &mut Bot, word: &str) {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            while tokio::time::Instant::now() < deadline
                && !bot.notices().iter().any(|text| text.starts_with(word))
            {
                let _ = tokio::time::timeout(Duration::from_millis(200), bot.recv()).await;
            }
        }

        // The listed block: Craft's listed handler answers, ahead of
        // Life — loaded first — and of Craft's own unlisted handler. If
        // Life's empty-hand `'watched'` ever won the race instead, the walk
        // would have stopped there and `'cooked'` would never show up: that
        // is what proves the listed slot is really asked first, not merely
        // that it is reachable at all.
        bot.use_block(centre_of(fire_pos)).await.expect("send");
        wait_for(&mut bot, "cooked").await;
        assert!(
            told(&bot, "cooked"),
            "the listed handler did not answer for its own block; notices {:?}",
            bot.notices()
        );
        assert!(
            !told(&bot, "watched"),
            "Life's unlisted handler answered ahead of Craft's listed one; notices {:?}",
            bot.notices()
        );

        // Off the listed block: Life declines (an empty hand), and Craft's
        // unlisted handler — heard in its ordinary load-order place —
        // answers instead.
        bot.use_block(centre_of(anvil_pos)).await.expect("send");
        wait_for(&mut bot, "stationed").await;
        assert!(
            told(&bot, "stationed"),
            "the unlisted handler did not answer off the listed block; notices {:?}",
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

#[test]
fn a_mod_told_of_a_sweep_can_refuse_it_and_the_floor_stays() {
    // Contract §7.6: a whole block laid on a floor one cell thick sweeps the
    // floor — unless a mod's `on_place` refuses, which it can because the
    // event says `swept`; then nothing is written and nothing is charged.
    // The rule that a bare hand may clear only loose ground is Craft's; this
    // proves the flag and the ordering it relies on.
    let server = start(
        "level-this-ground",
        write_warden(
            "level-this-ground",
            "game.register_block{ id = \"stump\", whole = true,\n\
             \x20   shape = { \"### ### ###\", \"... ... ...\", \"... ... ...\" } }\n\
             game.register_on_place(function(e)\n\
             \x20   if e.swept then return \"level this ground\" end\n\
             end)",
        ),
    );

    block_on(async {
        let mut bot = join(&server).await;
        let stump = material_named(&bot, "warden:stump");
        let ground = material_named(&bot, "warden:ground");

        // A stump in hand: seeded whole, dug whole by the hand.
        let source = BlockPos::new(2, 0, 0);
        assert!(server.seed_block(source, stump), "seed queue full");
        let seen_at = |bot: &Bot, at: BlockPos, material: u16| {
            bot.received().iter().any(|message| {
                matches!(
                    message,
                    tiamat_core::proto::ServerMessage::BlockDelta {
                        edit: tiamat_core::proto::Edit::Partial { pos, material: got, .. },
                        ..
                    } if *pos == at && *got == material
                )
            })
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !seen_at(&bot, source, stump) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the stump never landed"
            );
            let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
        }
        // Aimed at a cell OF the stump — its layer is the bottom one, and
        // the block's centre is air — so the hand's dig becomes a dig of the
        // whole (Contract §7.5).
        let foot = SubNodePos::new(source.x * 3 + 1, source.y * 3, source.z * 3 + 1);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while bot.units_of(stump) < 27 {
            bot.start_dig(foot).await.expect("start dig");
            let _ = bot.await_inventory(Duration::from_secs(2)).await;
            assert!(
                tokio::time::Instant::now() < deadline,
                "the stump never came up"
            );
        }

        // A floor one cell thick, of the ground a mod may protect.
        let floor = BlockPos::new(-2, 0, 0);
        let bottom: u32 = (0..3)
            .flat_map(|x| (0..3).map(move |z| 1 << tiamat_core::block::subnode_index(x, 0, z)))
            .sum();
        assert!(
            server.seed_partial(floor, ground, bottom),
            "seed queue full"
        );
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !seen_at(&bot, floor, ground) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the floor never landed"
            );
            let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
        }

        // The stump laid on it: a sweep, which the mod refuses.
        bot.hold_brush("block").await.expect("hold the block brush");
        let asked = SubNodePos::new(floor.x * 3 + 1, floor.y * 3 + 1, floor.z * 3 + 1);
        bot.place_shape_against(asked, stump, 0, [0, 1, 0])
            .await
            .expect("ask to place");
        tokio::time::sleep(Duration::from_millis(500)).await;
        let _ = tokio::time::timeout(Duration::from_millis(500), bot.recv()).await;
        assert!(
            bot.notices().iter().any(|text| text == "level this ground"),
            "the refusal was not the mod's words; notices {:?}",
            bot.notices()
        );
        assert!(
            !seen_at(&bot, floor, stump) && bot.cells_broken(floor) == 0,
            "the refused sweep wrote something: the floor was touched"
        );
        assert_eq!(
            bot.units_of(stump),
            27,
            "a refused placement charged the player"
        );
    });

    assert!(server.stop());
}

#[test]
fn a_placement_into_a_whole_block_reaches_the_mods_before_the_refusal() {
    // Contract §7.5: nothing is written into a whole material's block, but
    // the mods hear the attempt first, so a torch held to a laid campfire
    // lights it (Craft). Here the hook makes an action of a block of ground
    // laid against a stump — "lit" — and nothing is written; without a hook
    // the same placement is refused as one piece.
    let server = start(
        "torch-to-campfire",
        write_warden(
            "torch-to-campfire",
            "game.register_block{ id = \"stump\", whole = true,\n\
             \x20   shape = { \"### ### ###\", \"... ... ...\", \"... ... ...\" } }\n\
             game.register_on_place(function(e)\n\
             \x20   local at = game.get_block{ x = e.x, y = e.y, z = e.z }\n\
             \x20   if at and game.block_of(at.material) == \"warden:stump\" then return \"lit\" end\n\
             end)",
        ),
    );

    block_on(async {
        let mut bot = join(&server).await;
        let stump = material_named(&bot, "warden:stump");
        let ground = material_named(&bot, "warden:ground");

        // A stump standing at ground level, and a block of ground in hand.
        let fire = BlockPos::new(2, 0, 0);
        assert!(server.seed_block(fire, stump), "seed queue full");
        let seen_at = |bot: &Bot, at: BlockPos, material: u16| {
            bot.received().iter().any(|message| {
                matches!(
                    message,
                    tiamat_core::proto::ServerMessage::BlockDelta {
                        edit: tiamat_core::proto::Edit::Partial { pos, material: got, .. },
                        ..
                    } if *pos == at && *got == material
                )
            })
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !seen_at(&bot, fire, stump) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the stump never landed"
            );
            let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
        }
        let quarry = BlockPos::new(-2, -1, 0);
        assert!(
            dig_and_see(&mut bot, &server, quarry, ground).await,
            "digging ground should work"
        );
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while bot.units_of(ground) < 27 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the ground never came up"
            );
            let _ = bot.await_inventory(Duration::from_millis(200)).await;
        }

        // Ground placed against the stump's top: the hook hears it, says
        // "lit", and the stump's block is untouched.
        bot.hold_brush("block").await.expect("hold the block brush");
        let asked = SubNodePos::new(fire.x * 3 + 1, fire.y * 3 + 1, fire.z * 3 + 1);
        bot.place_shape_against(asked, ground, 0, [0, 1, 0])
            .await
            .expect("ask to place");
        tokio::time::sleep(Duration::from_millis(500)).await;
        let _ = tokio::time::timeout(Duration::from_millis(500), bot.recv()).await;
        assert!(
            bot.notices().iter().any(|text| text == "lit"),
            "the hook never heard the placement; notices {:?}",
            bot.notices()
        );
        assert!(
            !bot.notices().iter().any(|text| text.contains("one piece")),
            "the engine refused before the mod could act; notices {:?}",
            bot.notices()
        );
        assert!(
            !seen_at(&bot, fire, ground) && bot.cells_broken(fire) == 0,
            "something was written into the stump's block"
        );
        assert_eq!(bot.units_of(ground), 27, "the player was charged");
    });

    assert!(server.stop());
}

#[test]
fn a_whole_block_that_does_not_sweep_stands_among_a_thin_floors_cells() {
    // Contract §7.6, Craft ask 14: a whole block registered `sweeps = false`
    // — a torch — laid on a floor one cell thick does not sweep the floor;
    // it takes the air cells of its shape among the floor's, and the floor
    // stays. A peg whose shape is the centre column: its bottom cell is the
    // floor's and stays ground, its two upper cells are written.
    let server = start(
        "peg-in-thin-floor",
        write_warden(
            "peg-in-thin-floor",
            "game.register_block{ id = \"peg\", whole = true, sweeps = false,\n\
             \x20   shape = { \"... .#. ...\", \"... .#. ...\", \"... .#. ...\" } }",
        ),
    );

    block_on(async {
        let mut bot = join(&server).await;
        let peg = material_named(&bot, "warden:peg");
        let ground = material_named(&bot, "warden:ground");
        let seen_at = |bot: &Bot, at: BlockPos, material: u16| {
            bot.received().iter().any(|message| {
                matches!(
                    message,
                    tiamat_core::proto::ServerMessage::BlockDelta {
                        edit: tiamat_core::proto::Edit::Partial { pos, material: got, .. },
                        ..
                    } if *pos == at && *got == material
                )
            })
        };

        // A peg in hand: seeded whole, dug whole by the hand at its foot.
        let source = BlockPos::new(2, 0, 0);
        assert!(server.seed_block(source, peg), "seed queue full");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !seen_at(&bot, source, peg) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the peg never landed"
            );
            let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
        }
        let foot = SubNodePos::new(source.x * 3 + 1, source.y * 3, source.z * 3 + 1);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while bot.units_of(peg) < 27 {
            bot.start_dig(foot).await.expect("start dig");
            let _ = bot.await_inventory(Duration::from_secs(2)).await;
            assert!(
                tokio::time::Instant::now() < deadline,
                "the peg never came up"
            );
        }

        // A floor one cell thick, and the peg laid on it.
        let floor = BlockPos::new(-2, 0, 0);
        let bottom: u32 = (0..3)
            .flat_map(|x| (0..3).map(move |z| 1 << tiamat_core::block::subnode_index(x, 0, z)))
            .sum();
        assert!(
            server.seed_partial(floor, ground, bottom),
            "seed queue full"
        );
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !seen_at(&bot, floor, ground) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the floor never landed"
            );
            let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
        }
        bot.hold_brush("block").await.expect("hold the block brush");
        let asked = SubNodePos::new(floor.x * 3 + 1, floor.y * 3 + 1, floor.z * 3 + 1);
        bot.place_shape_against(asked, peg, 0, [0, 1, 0])
            .await
            .expect("ask to place");

        // The peg's two upper cells arrive one edit each, among the floor;
        // nothing of the floor goes, and nothing replaces the block.
        let cell_written = |bot: &Bot, at: SubNodePos| {
            bot.received().iter().any(|message| {
                matches!(
                    message,
                    tiamat_core::proto::ServerMessage::BlockDelta {
                        edit: tiamat_core::proto::Edit::SubNode { pos, material },
                        ..
                    } if *pos == at && *material == peg
                )
            })
        };
        let middle = SubNodePos::new(floor.x * 3 + 1, floor.y * 3 + 1, floor.z * 3 + 1);
        let top = SubNodePos::new(floor.x * 3 + 1, floor.y * 3 + 2, floor.z * 3 + 1);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !(cell_written(&bot, middle) && cell_written(&bot, top)) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the peg never stood in the floor; notices {:?}",
                bot.notices()
            );
            let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
        }
        assert!(
            !seen_at(&bot, floor, peg) && bot.cells_broken(floor) == 0,
            "the floor was swept or replaced"
        );
        // The debit arrives on its own message, after the deltas.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while bot.units_of(peg) != 0 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "a whole block's units were not paid: {} left",
                bot.units_of(peg)
            );
            let _ = bot.await_inventory(Duration::from_millis(200)).await;
        }
    });

    assert!(server.stop());
}
