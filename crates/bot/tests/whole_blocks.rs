// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Whole blocks over a real server: Sub-Node Contract §7.5, driven through the
//! reference mods' `core:brazier` (a model block, whole by implication) and
//! `core:anvil` (whole and nothing else).
//!
//! A chisel aimed at a brazier takes the brazier — in one edit, for a whole
//! block's units, however few cells its shape has — and a brazier is placed
//! back as its shape whatever the bot holds, into an empty block only; nothing
//! is ever written into its block.

use std::time::Duration;

use bot::Bot;
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::interest::ViewDistance;
use tiamat_core::proto::{Edit, ServerMessage};
use tiamat_core::{BlockPos, MaterialId, SubNodePos, UNITS_PER_BLOCK};
use tiamat_server::{ServerHandle, Settings};

/// The reference mods, which define the brazier, the anvil and the chisel.
fn reference_mods() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../game")
        .canonicalize()
        .expect("the reference mods live at the repo root")
}

fn start(name: &str) -> ServerHandle {
    let dir = std::env::temp_dir().join("tiamat-whole-blocks").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    ServerHandle::start(&Settings {
        world_options: Vec::new(),
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: dir,
        identity_path: None,
        max_players: 8,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(reference_mods()),
        enabled_mods: bot::fixture::enabled_mods_for(&reference_mods())
            .expect("the reference mods' manifests"),
        seed: Some(5),
        rcon: None,
        materials: Vec::new(),
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
    bot.join("Smith").await.expect("join");
    bot
}

fn wire_id(bot: &Bot, name: &str) -> u16 {
    bot.material_table()
        .expect("a material table")
        .into_iter()
        .find(|entry| entry.name == name)
        .map(|entry| entry.id)
        .unwrap_or_else(|| panic!("`{name}` is not in the material table"))
}

/// The brazier's shape as `core_blocks/init.lua` declares it: the foot (the
/// bottom layer), the stem (the centre cell) and the bowl (the top layer).
fn brazier_shape() -> u32 {
    let mut mask = 1 << tiamat_core::block::subnode_index(1, 1, 1);
    for x in 0..3 {
        for z in 0..3 {
            mask |= 1 << tiamat_core::block::subnode_index(x, 0, z);
            mask |= 1 << tiamat_core::block::subnode_index(x, 2, z);
        }
    }
    mask
}

/// The last partial write this bot saw for `pos`, as its mask.
fn partial_seen(bot: &Bot, pos: BlockPos, material: u16) -> Option<u32> {
    bot.received()
        .iter()
        .rev()
        .find_map(|message| match message {
            ServerMessage::BlockDelta {
                edit:
                    Edit::Partial {
                        pos: got,
                        material: got_material,
                        occupancy,
                    },
                ..
            } if *got == pos && *got_material == material => Some(*occupancy),
            _ => None,
        })
}

/// Whether any single cell of `pos` was ever edited to air: the crumble a
/// whole block must never do.
fn saw_a_cell_go(bot: &Bot, pos: BlockPos) -> bool {
    bot.received().iter().any(|message| {
        matches!(
            message,
            ServerMessage::BlockDelta {
                edit: Edit::SubNode { pos: cell, material },
                ..
            } if cell.block() == pos && *material == MaterialId::AIR.0
        )
    })
}

async fn wait_for(bot: &mut Bot, what: &str, mut happy: impl FnMut(&Bot) -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !happy(bot) {
        assert!(tokio::time::Instant::now() < deadline, "never saw {what}");
        let _ = bot.await_inventory(Duration::from_millis(100)).await;
    }
}

/// Digs at `target` with the chisel until the block is air.
async fn chisel_until_gone(bot: &mut Bot, target: SubNodePos) {
    bot.select_tool(Some("core_tools:chisel"))
        .await
        .expect("select chisel");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        bot.start_dig(target).await.expect("start dig");
        if bot
            .expect_block(target.block(), MaterialId::AIR.0, Duration::from_secs(4))
            .await
            .is_ok()
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "chiselling {target:?} never took the block"
        );
    }
}

#[test]
fn a_chisel_takes_a_brazier_whole_in_one_edit_for_a_whole_blocks_units() {
    // Contract §7.5 end to end. Seeded as a whole block, it lands as its shape
    // (the seed queue writes a whole material as its shape); chiselled at one
    // cell of its foot, the WHOLE block goes, in one edit and never a cell at a
    // time; and the bot is paid 27 units for a block of nineteen cells.
    let server = start("chisel-takes-whole");
    block_on(async {
        let mut bot = join(&server).await;
        let brazier = wire_id(&bot, "core:brazier");
        let at = BlockPos::new(2, 1, 0);

        assert!(server.seed_block(at, brazier), "seed queue full");
        wait_for(&mut bot, "the brazier's shape", |bot| {
            partial_seen(bot, at, brazier) == Some(brazier_shape())
        })
        .await;

        let before = bot.units_of(brazier);
        // The centre of the foot: an occupied cell a chisel would take alone
        // of anything else.
        let foot = SubNodePos::new(at.x * 3 + 1, at.y * 3, at.z * 3 + 1);
        chisel_until_gone(&mut bot, foot).await;

        assert!(
            !saw_a_cell_go(&bot, at),
            "a whole block crumbled cell by cell; it comes off in one edit"
        );
        wait_for(&mut bot, "27 units of brazier", |bot| {
            bot.units_of(brazier).saturating_sub(before) >= UNITS_PER_BLOCK
        })
        .await;
        assert_eq!(
            bot.units_of(brazier) - before,
            UNITS_PER_BLOCK,
            "a whole block pays a whole block, not one unit per cell of its shape"
        );
        bot.disconnect().await;
    });
    assert!(server.stop(), "clean shutdown");
}

#[test]
fn a_brazier_is_placed_as_its_shape_whatever_is_held_and_nothing_goes_into_it() {
    // Contract §7.5: with the CHISEL held — a sub-node brush — 27 units of
    // brazier go down as the brazier's shape, all at once, for all 27; and a
    // chisel then trying to fill one of its empty cells with stone is refused
    // as "one piece", the cell staying empty.
    let server = start("brazier-placed-as-shape");
    block_on(async {
        let mut bot = join(&server).await;
        let brazier = wire_id(&bot, "core:brazier");
        let white = wire_id(&bot, "core:white");

        // Something to hold: a brazier, dug up whole, and a block of white.
        let source = BlockPos::new(2, 1, 0);
        assert!(server.seed_block(source, brazier));
        wait_for(&mut bot, "the seeded brazier", |bot| {
            partial_seen(bot, source, brazier).is_some()
        })
        .await;
        let foot = SubNodePos::new(source.x * 3 + 1, source.y * 3, source.z * 3 + 1);
        chisel_until_gone(&mut bot, foot).await;
        let quarry = BlockPos::new(3, 1, 0);
        assert!(server.seed_block(quarry, white));
        bot.expect_block(quarry, white, Duration::from_secs(10))
            .await
            .expect("the white block is seeded");
        bot.dig_block(quarry).await.expect("dig the white block");
        wait_for(&mut bot, "a brazier and some white in hand", |bot| {
            bot.units_of(brazier) >= UNITS_PER_BLOCK && bot.units_of(white) > 0
        })
        .await;

        // Placed with the chisel in hand, aimed at one cell of an empty block.
        let at = BlockPos::new(2, 2, 0);
        bot.hold_brush("subnode").await.expect("hold the chisel");
        let aim = SubNodePos::new(at.x * 3, at.y * 3, at.z * 3);
        let held = bot.units_of(brazier);
        bot.place_from_inventory(aim, brazier)
            .await
            .expect("ask to place");
        wait_for(&mut bot, "the brazier's shape where it was placed", |bot| {
            partial_seen(bot, at, brazier) == Some(brazier_shape())
        })
        .await;
        wait_for(&mut bot, "27 units spent", |bot| {
            bot.units_of(brazier) + UNITS_PER_BLOCK <= held
        })
        .await;
        assert_eq!(held - bot.units_of(brazier), UNITS_PER_BLOCK);

        // Now a chisel's cell of white into one of the brazier's empty cells —
        // a corner of the middle layer, beside the stem.
        let gap = SubNodePos::new(at.x * 3, at.y * 3 + 1, at.z * 3);
        let white_held = bot.units_of(white);
        bot.place_from_inventory(gap, white)
            .await
            .expect("ask to place");
        wait_for(&mut bot, "the refusal", |bot| {
            bot.notices()
                .iter()
                .any(|notice| notice.contains("one piece"))
        })
        .await;
        assert!(
            !bot.saw_subnode(gap, white),
            "a cell was written into a whole material's block"
        );
        assert_eq!(bot.units_of(white), white_held, "a refusal costs nothing");
        bot.disconnect().await;
    });
    assert!(server.stop(), "clean shutdown");
}

#[test]
fn an_anvil_is_whole_without_a_model() {
    // `whole = true` alone: the chisel takes the cube in one edit for 27.
    let server = start("anvil-whole");
    block_on(async {
        let mut bot = join(&server).await;
        let anvil = wire_id(&bot, "core:anvil");
        let at = BlockPos::new(2, 1, 0);
        assert!(server.seed_block(at, anvil));
        bot.expect_block(at, anvil, Duration::from_secs(10))
            .await
            .expect("the anvil is seeded whole");
        let corner = SubNodePos::new(at.x * 3, at.y * 3, at.z * 3);
        chisel_until_gone(&mut bot, corner).await;
        assert!(!saw_a_cell_go(&bot, at), "an anvil crumbled");
        wait_for(&mut bot, "27 units of anvil", |bot| {
            bot.units_of(anvil) >= UNITS_PER_BLOCK
        })
        .await;
        assert_eq!(bot.units_of(anvil), UNITS_PER_BLOCK);
        bot.disconnect().await;
    });
    assert!(server.stop(), "clean shutdown");
}
