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

/// A block as it should read, cell by cell, from the masks of what is in it:
/// `layers` are (material, mask), a later layer over an earlier one, and a
/// cell in none is air.
fn layout(layers: &[(u16, u32)]) -> [u16; 27] {
    let mut cells = [0u16; 27];
    for (material, mask) in layers {
        for (slot, cell) in cells.iter_mut().enumerate() {
            if mask & (1 << slot) != 0 {
                *cell = *material;
            }
        }
    }
    cells
}

/// Waits until every cell of `pos` reads as `want`, and says which cell is
/// wrong if it never does.
///
/// **The whole layout, not a cell of it.** A mixed block is written one
/// `SubNode` delta per cell, and a check that ran once the first of them had
/// arrived read the rest as still air — a bowl cell of the brazier, on a
/// Windows runner slow enough to pump an inventory update between two of
/// the deltas. Waiting for the layout is waiting for the last of them.
async fn expect_cells(bot: &mut Bot, pos: BlockPos, want: [u16; 27], what: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while cells_seen(bot, pos) != want {
        if tokio::time::Instant::now() >= deadline {
            let cells = cells_seen(bot, pos);
            for (slot, cell) in cells.iter().enumerate() {
                assert_eq!(*cell, want[slot], "{what}: cell {slot}");
            }
            panic!("{what}: never saw the layout");
        }
        let _ = bot.await_inventory(Duration::from_millis(100)).await;
    }
}

/// Digs up a seeded brazier whole, so the bot holds one. Returns the
/// brazier's and white's wire ids.
async fn brazier_in_hand(bot: &mut Bot, server: &ServerHandle) -> (u16, u16) {
    let brazier = wire_id(bot, "core:brazier");
    let white = wire_id(bot, "core:white");
    let source = BlockPos::new(2, 1, 0);
    assert!(server.seed_block(source, brazier));
    wait_for(bot, "the seeded brazier", |bot| {
        partial_seen(bot, source, brazier).is_some()
    })
    .await;
    chisel_until_gone(
        bot,
        SubNodePos::new(source.x * 3 + 1, source.y * 3, source.z * 3 + 1),
    )
    .await;
    wait_for(bot, "a brazier in hand", |bot| {
        bot.units_of(brazier) >= UNITS_PER_BLOCK
    })
    .await;
    (brazier, white)
}

/// A wall one cell thick along a block's `x = 0` face.
fn thin_wall() -> u32 {
    (0..3)
        .flat_map(|y| (0..3).map(move |z| (y, z)))
        .map(|(y, z)| 1 << tiamat_core::block::subnode_index(0, y, z))
        .fold(0, |mask, bit| mask | bit)
}

/// Stands a brazier in the block of a thin wall at `wall`, placed against
/// the wall's inner face (a side face: Contract §7.5's "the air cells of its
/// shape", with the wall's cells kept) — the mixed block a swap is about.
/// Returns the brazier's and the wall's wire ids.
async fn brazier_beside_a_thin_wall(
    bot: &mut Bot,
    server: &ServerHandle,
    wall: BlockPos,
) -> (u16, u16) {
    let (brazier, white) = brazier_in_hand(bot, server).await;
    assert!(server.seed_partial(wall, white, thin_wall()));
    wait_for(bot, "the thin wall", |bot| {
        partial_seen(bot, wall, white) == Some(thin_wall())
    })
    .await;
    bot.hold_brush("block").await.expect("hold the block brush");
    // The cell across the wall's inner (+x) face, inside the wall's block.
    let asked = SubNodePos::new(wall.x * 3 + 1, wall.y * 3, wall.z * 3 + 1);
    bot.place_shape_against(asked, brazier, 0, [1, 0, 0])
        .await
        .expect("ask to place");
    expect_cells(
        bot,
        wall,
        layout(&[
            (white, thin_wall()),
            (brazier, brazier_shape() & !thin_wall()),
        ]),
        "the brazier beside the wall",
    )
    .await;
    wait_for(bot, "27 units spent", |bot| bot.units_of(brazier) == 0).await;
    (brazier, white)
}

#[test]
fn a_brazier_set_on_a_thin_floor_sweeps_the_floor_and_stands_on_the_block_beneath() {
    // Contract §7.6 (the designer, 2026-10-08): to a whole material a block
    // with no node in its top layer is not ground. Placed against the top
    // of a floor one cell thick, the brazier's shape is written whole at
    // that block's bottom and the floor's nine cells are gone. Dug, it comes
    // up alone and the block is empty. With one node in the top layer the
    // floor is ground: the brazier goes in the block above, intact, and the
    // floor stays.
    let server = start("brazier-sweeps-thin-floor");
    block_on(async {
        let mut bot = join(&server).await;
        let (brazier, white) = brazier_in_hand(&mut bot, &server).await;

        let floor = BlockPos::new(2, 1, 1);
        assert!(server.seed_partial(floor, white, thin_floor()));
        wait_for(&mut bot, "the thin floor", |bot| {
            partial_seen(bot, floor, white) == Some(thin_floor())
        })
        .await;
        bot.hold_brush("block").await.expect("hold the block brush");
        // Aimed across the top face of the floor's top cell, as a client
        // aims: the asked cell is inside the floor's own block.
        let asked = SubNodePos::new(floor.x * 3 + 1, floor.y * 3 + 1, floor.z * 3 + 1);
        bot.place_shape_against(asked, brazier, 0, [0, 1, 0])
            .await
            .expect("ask to place");
        expect_cells(
            &mut bot,
            floor,
            layout(&[(brazier, brazier_shape())]),
            "the brazier whole where the floor was",
        )
        .await;
        assert!(
            cells_seen(&bot, BlockPos::new(floor.x, floor.y + 1, floor.z))
                .iter()
                .all(|cell| *cell == 0),
            "nothing floated into the block above"
        );
        wait_for(&mut bot, "27 units spent", |bot| bot.units_of(brazier) == 0).await;

        // Dug with the chisel at its stem: it comes up alone, for 27 units,
        // and the block is empty — the floor went when it was laid.
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
        expect_cells(&mut bot, floor, [0u16; 27], "an empty block after the dig").await;
        assert_eq!(bot.units_of(brazier), UNITS_PER_BLOCK, "a whole block paid");

        // One node in the top layer makes a floor ground: placed on, intact,
        // in the block above, and the floor untouched.
        let ledge = BlockPos::new(2, 1, 2);
        let with_top = thin_floor() | (1 << tiamat_core::block::subnode_index(1, 2, 1));
        assert!(server.seed_partial(ledge, white, with_top));
        wait_for(&mut bot, "the ledge", |bot| {
            partial_seen(bot, ledge, white) == Some(with_top)
        })
        .await;
        bot.hold_brush("block").await.expect("hold the block brush");
        let asked = SubNodePos::new(ledge.x * 3 + 1, ledge.y * 3 + 3, ledge.z * 3 + 1);
        bot.place_shape_against(asked, brazier, 0, [0, 1, 0])
            .await
            .expect("ask to place");
        let above = BlockPos::new(ledge.x, ledge.y + 1, ledge.z);
        expect_cells(
            &mut bot,
            above,
            layout(&[(brazier, brazier_shape())]),
            "the brazier on the ledge, in the block above",
        )
        .await;
        assert_eq!(
            cells_seen(&bot, ledge),
            layout(&[(white, with_top)]),
            "the ledge untouched"
        );
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

#[test]
fn a_whole_block_written_where_one_stands_beside_a_thin_wall_keeps_the_wall() {
    // Craft ask 12 (2026-10-07), Contract §7.5 "swapped in place": a mod's
    // `set_block` of a whole material on the block another stands in — a
    // campfire lit, a torch burnt out — replaces the thing, not the block.
    // The ground's cells stay; the write used to erase them (charter rule
    // 5). The ground here is a thin wall the brazier was placed beside by a
    // side face, the mixed block §7.5 still makes (a thin FLOOR is swept
    // now — §7.6).
    let server = start("brazier-swapped-beside-thin-wall");
    block_on(async {
        let mut bot = join(&server).await;
        let floor = BlockPos::new(2, 1, 1);
        let (brazier, white) = brazier_beside_a_thin_wall(&mut bot, &server, floor).await;
        let anvil = wire_id(&bot, "core:anvil");

        // `game.set_block(floor, "core:anvil")` is this queue. The anvil has
        // no `shape`, so it is a whole-block write — the form that replaced
        // the block outright.
        assert!(server.seed_block(floor, anvil), "seed queue full");
        expect_cells(
            &mut bot,
            floor,
            layout(&[
                (anvil, tiamat_core::block::OCCUPANCY_FULL),
                (white, thin_wall()),
            ]),
            "the anvil in the brazier's place, the wall kept",
        )
        .await;

        // And back to a shaped one: the anvil's cells the brazier's shape does
        // not reuse go to air, the floor's nine still stay.
        assert!(server.seed_block(floor, brazier), "seed queue full");
        expect_cells(
            &mut bot,
            floor,
            layout(&[
                (white, thin_wall()),
                (brazier, brazier_shape() & !thin_wall()),
            ]),
            "the brazier back in the anvil's place, the wall kept",
        )
        .await;

        // Where no ground shares the block, a replace is still a replace: a
        // seeded anvil, written over as a brazier, is exactly the brazier's
        // shape, one partial edit, and nothing of the anvil.
        let bare = BlockPos::new(2, 1, 3);
        assert!(server.seed_block(bare, anvil), "seed queue full");
        wait_for(&mut bot, "the bare anvil", |bot| {
            cells_seen(bot, bare).iter().all(|cell| *cell == anvil)
        })
        .await;
        assert!(server.seed_block(bare, brazier), "seed queue full");
        wait_for(&mut bot, "the brazier replacing the anvil", |bot| {
            partial_seen(bot, bare, brazier) == Some(brazier_shape())
        })
        .await;
        assert!(
            cells_seen(&bot, bare).iter().all(|cell| *cell != anvil),
            "nothing of the anvil stayed"
        );
        bot.disconnect().await;
    });
    assert!(server.stop(), "clean shutdown");
}
