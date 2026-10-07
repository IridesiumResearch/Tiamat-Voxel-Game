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

/// A floor one cell thick: the bottom layer of a block.
fn thin_floor() -> u32 {
    (0..3)
        .flat_map(|x| (0..3).map(move |z| (x, z)))
        .map(|(x, z)| 1 << tiamat_core::block::subnode_index(x, 0, z))
        .fold(0, |mask, bit| mask | bit)
}

/// What this bot has been told each cell of `pos` holds, from the deltas it
/// saw after the last whole-block write to it.
fn cells_seen(bot: &Bot, pos: BlockPos) -> [u16; 27] {
    let mut cells = [0u16; 27];
    for message in bot.received() {
        let ServerMessage::BlockDelta { edit, .. } = message else {
            continue;
        };
        match edit {
            Edit::Block { pos: got, material } if got == pos => cells = [material; 27],
            Edit::Partial {
                pos: got,
                material,
                occupancy,
            } if got == pos => {
                for (slot, cell) in cells.iter_mut().enumerate() {
                    *cell = if occupancy & (1 << slot) != 0 {
                        material
                    } else {
                        0
                    };
                }
            }
            Edit::SubNode {
                pos: cell,
                material,
            } if cell.block() == pos => {
                let (x, y, z) = (
                    cell.x.rem_euclid(3) as u32,
                    cell.y.rem_euclid(3) as u32,
                    cell.z.rem_euclid(3) as u32,
                );
                cells[tiamat_core::block::subnode_index(x, y, z)] = material;
            }
            _ => {}
        }
    }
    cells
}

#[test]
fn a_brazier_set_on_a_thin_floor_stands_in_it_and_comes_up_alone() {
    // Contract §7.6: a block a third full is not ground. Placed against its
    // top, the brazier goes INTO that block, taking the air cells of its
    // shape and leaving the floor's cells; its foot is the floor. Dug, it
    // comes up alone for 27 units, and the floor is still there. A block
    // brush aimed at the floor beside it takes the floor and leaves it.
    let server = start("brazier-on-thin-floor");
    block_on(async {
        let mut bot = join(&server).await;
        let brazier = wire_id(&bot, "core:brazier");
        let white = wire_id(&bot, "core:white");

        // A brazier in hand, dug up whole.
        let source = BlockPos::new(2, 1, 0);
        assert!(server.seed_block(source, brazier));
        wait_for(&mut bot, "the seeded brazier", |bot| {
            partial_seen(bot, source, brazier).is_some()
        })
        .await;
        chisel_until_gone(
            &mut bot,
            SubNodePos::new(source.x * 3 + 1, source.y * 3, source.z * 3 + 1),
        )
        .await;
        wait_for(&mut bot, "a brazier in hand", |bot| {
            bot.units_of(brazier) >= UNITS_PER_BLOCK
        })
        .await;

        // A floor one cell thick at (2, 1, 1): nine cells of white.
        let floor = BlockPos::new(2, 1, 1);
        assert!(server.seed_partial(floor, white, thin_floor()));
        wait_for(&mut bot, "the thin floor", |bot| {
            partial_seen(bot, floor, white) == Some(thin_floor())
        })
        .await;

        // Placed against the top of the floor's top cell — the cell asked for
        // is in the block ABOVE, as a client's would be — the brazier lands
        // in the floor's block: its stem and bowl, with the foot's cells
        // staying the floor's.
        bot.hold_brush("block").await.expect("hold the block brush");
        let asked = SubNodePos::new(floor.x * 3 + 1, floor.y * 3 + 3, floor.z * 3 + 1);
        bot.place_shape_against(asked, brazier, 0, [0, 1, 0])
            .await
            .expect("ask to place");
        wait_for(&mut bot, "the brazier among the floor", |bot| {
            let cells = cells_seen(bot, floor);
            cells[tiamat_core::block::subnode_index(1, 1, 1)] == brazier
                && cells[tiamat_core::block::subnode_index(0, 0, 0)] == white
        })
        .await;
        let cells = cells_seen(&bot, floor);
        let expected = brazier_shape() & !thin_floor();
        for (slot, cell) in cells.iter().enumerate() {
            let want = if expected & (1 << slot) != 0 {
                brazier
            } else if thin_floor() & (1 << slot) != 0 {
                white
            } else {
                0
            };
            assert_eq!(*cell, want, "cell {slot}");
        }
        assert!(
            cells_seen(&bot, BlockPos::new(floor.x, floor.y + 1, floor.z))
                .iter()
                .all(|cell| *cell == 0),
            "nothing floated into the block above"
        );
        wait_for(&mut bot, "27 units spent", |bot| bot.units_of(brazier) == 0).await;

        // Dug with the chisel at its stem: the brazier's cells go, in one tick,
        // for 27 units; the floor's nine stay.
        let stem = SubNodePos::new(floor.x * 3 + 1, floor.y * 3 + 1, floor.z * 3 + 1);
        bot.select_tool(Some("core_tools:chisel"))
            .await
            .expect("chisel");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            bot.start_dig(stem).await.expect("start dig");
            let _ = bot.await_inventory(Duration::from_secs(2)).await;
            if bot.units_of(brazier) >= UNITS_PER_BLOCK {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the brazier never came up"
            );
        }
        wait_for(&mut bot, "the floor alone", |bot| {
            let cells = cells_seen(bot, floor);
            cells.iter().all(|cell| *cell != brazier)
        })
        .await;
        let cells = cells_seen(&bot, floor);
        for (slot, cell) in cells.iter().enumerate() {
            let want = if thin_floor() & (1 << slot) != 0 {
                white
            } else {
                0
            };
            assert_eq!(*cell, want, "after the dig, cell {slot}");
        }
        assert_eq!(bot.units_of(brazier), UNITS_PER_BLOCK, "a whole block paid");
        bot.disconnect().await;
    });
    assert!(server.stop(), "clean shutdown");
}

#[test]
fn ground_three_quarters_full_is_placed_on_and_a_thin_floor_is_filled_in() {
    // Contract §7.6, the other two cases: against the top of a block at
    // least 21 cells full, a thing goes in the block above as it always did;
    // loose material with a block brush against the top of a thin floor fills
    // the floor's gaps rather than starting a block above them.
    let server = start("ground-threshold");
    block_on(async {
        let mut bot = join(&server).await;
        let white = wire_id(&bot, "core:white");
        // Two blocks of white in hand: one to spend on each placement.
        for quarry in [BlockPos::new(3, 1, 0), BlockPos::new(3, 1, 1)] {
            assert!(server.seed_block(quarry, white));
            bot.expect_block(quarry, white, Duration::from_secs(10))
                .await
                .expect("seeded");
            bot.dig_block(quarry).await.expect("dig");
        }
        wait_for(&mut bot, "two blocks of white in hand", |bot| {
            bot.units_of(white) >= 2 * UNITS_PER_BLOCK
        })
        .await;

        // 21 cells: ground. Placed against its top (the top cell at y = 2
        // exists in a 21-cell bottom-up fill), the block above fills.
        let ground = BlockPos::new(2, 1, 2);
        let twenty_one = (1 << 21) - 1;
        assert!(server.seed_partial(ground, white, twenty_one));
        wait_for(&mut bot, "the ground", |bot| {
            partial_seen(bot, ground, white) == Some(twenty_one)
        })
        .await;
        bot.hold_brush("block").await.expect("block brush");
        let above = BlockPos::new(ground.x, ground.y + 1, ground.z);
        let asked = SubNodePos::new(ground.x * 3, ground.y * 3 + 3, ground.z * 3);
        let held = bot.units_of(white);
        bot.place_shape_against(asked, white, 0, [0, 1, 0])
            .await
            .expect("ask to place");
        wait_for(&mut bot, "a block above the ground", |bot| {
            cells_seen(bot, above).contains(&white)
        })
        .await;
        wait_for(&mut bot, "units spent", |bot| bot.units_of(white) < held).await;

        // A thin floor: loose white against its top fills ITS gaps.
        let thin = BlockPos::new(2, 1, 3);
        assert!(server.seed_partial(thin, white, thin_floor()));
        wait_for(&mut bot, "the thin floor", |bot| {
            partial_seen(bot, thin, white) == Some(thin_floor())
        })
        .await;
        let asked = SubNodePos::new(thin.x * 3 + 1, thin.y * 3 + 3, thin.z * 3 + 1);
        bot.place_shape_against(asked, white, 0, [0, 1, 0])
            .await
            .expect("ask to place");
        wait_for(&mut bot, "the thin floor filled in", |bot| {
            cells_seen(bot, thin)
                .iter()
                .filter(|cell| **cell == white)
                .count()
                > 9
        })
        .await;
        assert!(
            cells_seen(&bot, BlockPos::new(thin.x, thin.y + 1, thin.z))
                .iter()
                .all(|cell| *cell == 0),
            "loose material floated a block above a thin floor"
        );
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
