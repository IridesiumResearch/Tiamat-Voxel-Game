// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! A cut of several materials, end to end through the reference mods.
//!
//! Sub-Node Contract §9.1: one carved shape, each of its 27 cells its own
//! material, carried as one stack. The engine holds the mechanism — the stack,
//! the wire, the save, the placement cell by cell, the break back into loose
//! material — and the recipe is a mod's (charter rule 1). So these drive the
//! REFERENCE shape crafter, `game/core_ui`, and the reference drop and pick-up,
//! `game/core_gear`, unmodified, beside a small kit mod that supplies two
//! materials and somewhere flat to put them. Nothing here reaches past the
//! public API: a bot does what a player's client does, and the cut is painted,
//! paid for, made, placed, broken, dropped, saved and reloaded by Lua.
//!
//! Conservation (charter rule 5) is the thread through all of it: one item
//! costs one unit per cell of that cell's own material, and whatever happens
//! to the cut, the units of every material come back to what they were.

use std::path::{Path, PathBuf};
use std::time::Duration;

use bot::Bot;
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::interest::ViewDistance;
use tiamat_core::proto::{DialogEvent, StackDef};
use tiamat_core::ui::{Tree, Widget};
use tiamat_core::{BlockPos, SubNodePos};
use tiamat_server::{ServerHandle, Settings};

/// How long to wait for something the server has to tick before it is true.
const PATIENCE: Duration = Duration::from_secs(20);

/// The reference crafter's screen, as the server names it.
const FORM: &str = "core_ui:inventory";

/// Where the cut is placed: in reach, on the flat ground, and not where the
/// player stands — the block `(2, 1, 2)`, aimed at through its middle cell.
const AT: BlockPos = BlockPos::new(2, 1, 2);

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamat-mixed-cut").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(future)
}

fn game_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../game")
        .canonicalize()
        .expect("the reference mods live at the repository root")
}

/// Copies a mod directory, so a test's mod set holds the reference mod as it
/// is checked in rather than a version of it written for the test.
fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("mod dir");
    for entry in std::fs::read_dir(from).expect("read the mod") {
        let entry = entry.expect("entry");
        let path = entry.path();
        let target = to.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            std::fs::copy(&path, &target).expect("copy");
        }
    }
}

/// The two materials a cut is made of, a floor, and a hand to dig with.
///
/// `oak_first` registers oak before stone: the same mod, a later version, in
/// a different order — so the session's ids put oak below stone where the
/// world's put it above, and a cut's LOWEST material (§9.1 rule 2) is a
/// different one of its two in each session.
///
/// The materials are handed out on the chat word `kit` rather than on join,
/// because a join happens again after a restart and would add to what is
/// being counted. `probe` writes, at `y = 20 + id`, a block for each of the
/// session's ids for oak and stone, which is the only way a client can learn
/// a runtime id.
fn write_kit(root: &Path, oak_first: bool) {
    let dir = root.join("kit");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"kit\"\nname = \"Kit\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    let materials = if oak_first {
        "game.register_block{ id = \"oak\" }\ngame.register_block{ id = \"stone\" }\n"
    } else {
        "game.register_block{ id = \"stone\" }\ngame.register_block{ id = \"oak\" }\n"
    };
    let script = r#"
game.register_block{ id = "ground" }
@MATERIALS@
game.register_tool{ id = "hand", brush = "block", speed_multiplier = 1.0, default = true }
game.register_on_generate(function(buf, pos)
    buf:fill_below_heightmap(game.flat_heightmap(0), game.get_block_id("kit:ground"))
end)
game.register_on_chat(function(event)
    if event.text == "kit" then
        game.give(event.player, { material = "kit:stone", count = 1 })
        game.give(event.player, { material = "kit:oak", count = 1 })
        return false
    elseif event.text == "probe" then
        game.set_block({ x = 1, y = 20 + game.get_block_id("kit:oak"), z = 1 }, "kit:ground")
        game.set_block({ x = 3, y = 20 + game.get_block_id("kit:stone"), z = 1 }, "kit:ground")
        return false
    end
end)
"#
    .replace("@MATERIALS@", materials);
    std::fs::write(dir.join("init.lua"), script).expect("script");
}

/// A mod that loads in front of the kit and is later removed: five blocks,
/// so every id after them moves by five when it goes.
fn write_first(root: &Path) {
    let dir = root.join("first");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"first\"\nname = \"First\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        "for n = 1, 5 do game.register_block{ id = \"filler_\" .. n } end\n",
    )
    .expect("script");
}

/// The reference crafter and the reference drop, beside the kit.
fn mods(name: &str, with_first: bool, oak_first: bool) -> PathBuf {
    let root = scratch(name);
    for reference in ["core_ui", "core_gear"] {
        copy_dir(&game_dir().join(reference), &root.join(reference));
    }
    write_kit(&root, oak_first);
    if with_first {
        write_first(&root);
    }
    root
}

/// Starts a server on a world directory that may already exist.
fn start_at(mods: PathBuf, world: PathBuf) -> ServerHandle {
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
        seed: Some(11),
        rcon: None,
        materials: Vec::new(),
        world_options: Vec::new(),
    })
    .expect("start")
}

/// Joins under an identity the caller keeps, because the player who comes
/// back after a restart is the same player (charter rule 13).
async fn join_as(server: &ServerHandle, name: &str, identity: Identity) -> Bot {
    let mut bot = Bot::connect(server.local_addr(), identity, server.cert_fingerprint())
        .await
        .expect("connect");
    bot.join(name).await.expect("join");
    bot
}

/// A material's id as the client is told it: the WORLD id.
fn wire_id(bot: &Bot, name: &str) -> u16 {
    bot.material_table()
        .expect("a material table")
        .into_iter()
        .find(|entry| entry.name == name)
        .map(|entry| entry.id)
        .unwrap_or_else(|| panic!("`{name}` is not in the material table"))
}

/// Each cell's material by name, `""` for an empty cell.
fn names_of(bot: &Bot, cells: &[u16]) -> Vec<String> {
    let table = bot.material_table().expect("a material table");
    cells
        .iter()
        .map(|cell| {
            if *cell == 0 {
                return String::new();
            }
            table
                .iter()
                .find(|entry| entry.id == *cell)
                .map(|entry| entry.name.clone())
                .unwrap_or_else(|| panic!("world id {cell} is not in the material table"))
        })
        .collect()
}

/// Loose units of one material: no cut and no cells.
fn loose(stacks: &[StackDef], material: u16) -> u32 {
    stacks
        .iter()
        .filter(|stack| stack.material == material && stack.shape == 0 && stack.cells.is_empty())
        .map(|stack| stack.units)
        .sum()
}

/// The cut of several materials a player is carrying, if any.
fn carried_cut(stacks: &[StackDef]) -> Option<StackDef> {
    stacks.iter().find(|stack| !stack.cells.is_empty()).cloned()
}

/// The latest inventory, once it satisfies `happy`.
async fn inventory_where(
    bot: &mut Bot,
    what: &str,
    happy: impl Fn(&[StackDef]) -> bool,
) -> Vec<StackDef> {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let stacks = bot.inventory();
        if happy(&stacks) {
            return stacks;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what}: the inventory is {stacks:?}; the server said {:?}",
            bot.notices()
        );
        let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
    }
}

/// The crafter's tree, once it satisfies `happy`.
async fn screen_where(bot: &mut Bot, what: &str, happy: impl Fn(&Tree) -> bool) -> Tree {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        if let Some(tree) = bot.open_dialogs().remove(FORM)
            && happy(&tree)
        {
            return tree;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what}: the crafter shows {:?}",
            bot.open_dialogs().get(FORM)
        );
        let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
    }
}

/// The shape editor in a tree: its mask, its brush and its cells.
fn editor(tree: &Tree) -> Option<(u32, u16, Vec<u16>)> {
    tree.nodes.iter().find_map(|node| match &node.widget {
        Widget::ShapeEditor {
            shape,
            material,
            cells,
        } => Some((*shape, *material, cells.clone())),
        _ => None,
    })
}

/// Every label's text in a tree.
fn labels(tree: &Tree) -> Vec<String> {
    tree.nodes
        .iter()
        .filter_map(|node| match &node.widget {
            Widget::Label { text } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// The cut these tests make, in world ids: stone across the whole bottom
/// layer and a row of oak on its front edge above it — nine cells of stone
/// and three of oak, twelve units an item.
fn painted(stone: u16, oak: u16) -> Vec<u16> {
    let mut cells = vec![0u16; 27];
    for (index, cell) in cells.iter_mut().enumerate() {
        let y = index / 3 % 3;
        if y == 0 {
            *cell = stone;
        }
    }
    for cell in &mut cells[3..6] {
        *cell = oak;
    }
    cells
}

/// Which cells a run of cells fills.
fn occupancy(cells: &[u16]) -> u32 {
    cells
        .iter()
        .enumerate()
        .filter(|(_, cell)| **cell != 0)
        .fold(0, |mask, (index, _)| mask | (1 << index))
}

/// Asks the kit for a block of each material, opens the reference crafter in
/// several-material mode, paints `cells` and makes one: what a player does.
///
/// Returns the cut as the inventory then holds it.
async fn make(bot: &mut Bot, stone: u16, oak: u16, cells: &[u16]) -> StackDef {
    bot.chat("kit").await.expect("chat");
    inventory_where(bot, "the kit never arrived", |stacks| {
        loose(stacks, stone) >= 27 && loose(stacks, oak) >= 27
    })
    .await;

    bot.action("core_ui:inventory", true).await.expect("press");
    screen_where(bot, "the inventory never opened", |_| true).await;
    bot.press(FORM, "tab_shapes").await.expect("press");
    screen_where(bot, "the shapes tab never showed an editor", |tree| {
        editor(tree).is_some()
    })
    .await;

    // Several materials. The crafter starts from the block it was carving,
    // whole, in the material chosen — the first of what is carried by name,
    // which is oak.
    bot.dialog_event(
        FORM,
        DialogEvent::Toggled {
            name: "several".to_owned(),
            checked: true,
        },
    )
    .await
    .expect("toggle");
    let tree = screen_where(bot, "the editor never took several materials", |tree| {
        editor(tree).is_some_and(|(_, _, cells)| cells.len() == 27)
    })
    .await;
    let (_, brush, started) = editor(&tree).expect("an editor");
    assert_eq!(brush, oak, "the brush is not the material chosen");
    assert_eq!(
        started,
        vec![oak; 27],
        "several materials did not start whole"
    );

    // **Choosing a material sets the brush and nothing else.** The second
    // option is stone; the cells stay what they were.
    bot.dialog_event(
        FORM,
        DialogEvent::Chose {
            name: "material".to_owned(),
            index: 1,
        },
    )
    .await
    .expect("choose");
    let tree = screen_where(bot, "choosing stone never changed the brush", |tree| {
        editor(tree).is_some_and(|(_, brush, _)| brush == stone)
    })
    .await;
    assert_eq!(
        editor(&tree).map(|(_, _, cells)| cells),
        Some(started),
        "choosing a material changed the cells rather than the brush"
    );

    // What the client reports after a player has painted it: the mask and
    // every cell, in world ids, exactly as `dialog.rs` raises it.
    bot.dialog_event(
        FORM,
        DialogEvent::Chiselled {
            name: "cut".to_owned(),
            shape: occupancy(cells),
            cells: cells.to_vec(),
        },
    )
    .await
    .expect("paint");
    bot.press(FORM, "make").await.expect("make");

    let made = inventory_where(bot, "the cut never reached the inventory", |stacks| {
        carried_cut(stacks).is_some_and(|cut| cut.cells == cells)
    })
    .await;

    // **The cost line names each material's units**, as the crafter redraws
    // after a make: nine of stone and three of oak.
    screen_where(bot, "the cost line never named both materials", |tree| {
        labels(tree).iter().any(|text| {
            text.contains("9 units of kit:stone") && text.contains("3 units of kit:oak")
        })
    })
    .await;
    carried_cut(&made).expect("checked above")
}

/// Waits until the block at [`AT`], as the bot has been told it, is `want`.
async fn block_becomes(bot: &mut Bot, what: &str, want: &[u16]) -> tiamat_core::block::Cells {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let told = bot
            .block_as_told(AT)
            .expect("the chunk the cut is placed in");
        if told.iter().map(|cell| cell.0).eq(want.iter().copied()) {
            return told;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what}: the block holds {:?}; the server said {:?}",
            told.map(|cell| cell.0),
            bot.notices()
        );
        let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
    }
}

/// Places the cut as authored, and returns what the world then holds.
async fn place(bot: &mut Bot, cut: &StackDef) -> tiamat_core::block::Cells {
    // The chunk first: a bot that has just joined may not have been sent it.
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let before = loop {
        if let Ok(cells) = bot.block_as_told(AT) {
            break cells;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the chunk the cut is placed in never arrived"
        );
        let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
    };
    assert!(
        before.iter().all(|cell| cell.is_air()),
        "the block the cut is placed in was not empty to begin with"
    );
    // No face: as authored, so the cells land where they were painted.
    bot.place_stack_against(
        SubNodePos::new(AT.x * 3 + 1, AT.y * 3 + 1, AT.z * 3 + 1),
        cut,
        [0; 3],
    )
    .await
    .expect("place");
    block_becomes(bot, "the cut never landed as painted", &cut.cells).await
}

/// Breaks the block at [`AT`] with the kit's hand, and waits until `done`.
async fn break_it(bot: &mut Bot, done: impl Fn(&[StackDef]) -> bool) -> Vec<StackDef> {
    bot.hold_brush("block").await.expect("the kit's hand");
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        // Aimed at a cell that is filled, re-sent each round as `dig_block`
        // does, since re-aiming at the same cell keeps its progress.
        bot.start_dig(SubNodePos::new(AT.x * 3, AT.y * 3, AT.z * 3))
            .await
            .expect("dig");
        let round = tokio::time::Instant::now() + Duration::from_secs(3);
        while tokio::time::Instant::now() < round {
            let stacks = bot.inventory();
            if done(&stacks) {
                return stacks;
            }
            let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "breaking the cut never gave its materials back: {:?}; the server said {:?}",
            bot.inventory(),
            bot.notices()
        );
    }
}

#[test]
fn a_cut_of_two_materials_is_painted_made_placed_and_broken_back_into_its_materials() {
    // The whole of §9.1 through one player: paint, pay, make, carry, place,
    // read the block back as the Mixed block it is, break it, and hold the
    // loose units of each material again.
    let server = start_at(mods("made-mods", false, false), scratch("made-world"));
    block_on(async {
        let mut bot = join_as(&server, "Mason", Identity::generate().expect("identity")).await;
        let (stone, oak) = (wire_id(&bot, "kit:stone"), wire_id(&bot, "kit:oak"));
        let cells = painted(stone, oak);

        let cut = make(&mut bot, stone, oak, &cells).await;
        assert_eq!(cut.cells, cells, "the cut is not what was painted");
        assert_eq!(cut.units, 12, "one item of twelve cells is twelve units");
        assert_eq!(cut.shape, occupancy(&cells), "its shape is its occupancy");
        assert_eq!(
            cut.material,
            stone.min(oak),
            "its material is the lowest in it"
        );
        let held = bot.inventory();
        assert_eq!(
            (loose(&held, stone), loose(&held, oak)),
            (27 - 9, 27 - 3),
            "making one did not take one unit per cell of each cell's own material"
        );

        let placed = place(&mut bot, &cut).await;
        assert!(
            matches!(
                tiamat_core::block::BlockValue::Cells(placed).canonical(),
                tiamat_core::block::BlockValue::Cells(_)
            ),
            "the placed block is not Mixed: {placed:?}"
        );
        inventory_where(&mut bot, "placing did not spend the cut", |stacks| {
            carried_cut(stacks).is_none()
        })
        .await;

        let after = break_it(&mut bot, |stacks| {
            loose(stacks, stone) == 27 && loose(stacks, oak) == 27
        })
        .await;
        assert!(
            carried_cut(&after).is_none(),
            "breaking it gave back a cut rather than its materials: {after:?}"
        );
        block_becomes(&mut bot, "the block was not emptied", &[0; 27]).await;

        bot.disconnect().await;
    });
    assert!(server.stop());
}

#[test]
fn a_cut_of_two_materials_dropped_on_the_ground_is_the_same_cut_after_a_restart() {
    // The reference drop, the entity save, and the reference pick-up of a
    // stack that is nothing but its cells.
    let world = scratch("dropped-world");
    let mods = mods("dropped-mods", false, false);
    let seed = Identity::generate().expect("identity").seed();
    let cells;

    {
        let server = start_at(mods.clone(), world.clone());
        cells = block_on(async {
            let mut bot = join_as(&server, "Dropper", Identity::from_seed(&seed)).await;
            let (stone, oak) = (wire_id(&bot, "kit:stone"), wire_id(&bot, "kit:oak"));
            let cells = painted(stone, oak);
            make(&mut bot, stone, oak, &cells).await;

            // Held, and thrown with the key the reference mod registered.
            let slot = bot
                .view("player:main")
                .expect("the main view")
                .iter()
                .position(|slot| slot.as_ref().is_some_and(|stack| stack.cells == cells))
                .expect("the cut is in a slot");
            assert!(slot < 9, "the cut is not on the hotbar, in slot {slot}");
            bot.select_slot(u16::try_from(slot).expect("a hotbar slot"))
                .await
                .expect("select");
            bot.action("core_gear:drop", true).await.expect("drop");
            bot.expect_entity(
                |entity| {
                    entity
                        .item
                        .as_ref()
                        .is_some_and(|stack| stack.cells == cells)
                },
                PATIENCE,
            )
            .await
            .expect("the cut never lay on the ground as itself");
            inventory_where(&mut bot, "dropping it did not take it", |stacks| {
                carried_cut(stacks).is_none()
            })
            .await;

            // Walk away before the world closes, as `gear.rs` does: the
            // dropper may pick it back up once it has settled.
            for _ in 0..40 {
                let _ = bot.walk([0.0, 0.0, -1.0], 0, 4).await;
            }
            assert!(
                carried_cut(&bot.inventory()).is_none(),
                "the cut was picked back up before the world closed, so the restart proves \
                 nothing"
            );
            bot.disconnect().await;
            cells
        });
        assert!(server.stop(), "the world did not flush cleanly");
    }

    let server = start_at(mods, world);
    block_on(async {
        let mut bot = join_as(&server, "Dropper", Identity::from_seed(&seed)).await;
        let lying = bot
            .expect_entity(
                |entity| {
                    entity
                        .item
                        .as_ref()
                        .is_some_and(|stack| stack.cells == cells)
                },
                PATIENCE,
            )
            .await
            .expect("the cut on the ground did not survive the restart as itself");

        let deadline = tokio::time::Instant::now() + PATIENCE;
        loop {
            if let Some(cut) = carried_cut(&bot.inventory()) {
                assert_eq!(cut.cells, cells, "what was picked up is not the cut");
                assert_eq!(cut.units, 12, "picking it up gained or lost units");
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the cut survived the restart but was never picked up"
            );
            let at = bot
                .entities()
                .into_values()
                .find(|entity| {
                    entity
                        .item
                        .as_ref()
                        .is_some_and(|stack| stack.cells == cells)
                })
                .unwrap_or_else(|| lying.clone());
            let per_block = 3.0_f32;
            let span = tiamat_core::CHUNK_BLOCKS as f32;
            let _ = bot
                .move_to(
                    at.chunk.x as f32 * span + at.local[0] / per_block,
                    0.0,
                    at.chunk.z as f32 * span + at.local[2] / per_block,
                )
                .await;
            bot.sleep_ticks(4).await;
        }
        bot.disconnect().await;
    });
    assert!(server.stop());
}

/// The session's own id for oak and for stone, read from `probe`.
async fn runtime_ids(bot: &mut Bot) -> (i32, i32) {
    bot.chat("probe").await.expect("chat");
    let ground = wire_id(bot, "kit:ground");
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let at = |x: i32| (20..60).find(|y| bot.saw_block(BlockPos::new(x, *y, 1), ground));
        if let (Some(oak), Some(stone)) = (at(1), at(3)) {
            return (oak - 20, stone - 20);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the probe never reported the session's ids"
        );
        let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
    }
}

#[test]
fn a_cut_of_two_materials_is_still_what_it_was_made_of_under_a_changed_mod_set() {
    // Charter rule 8, twenty-seven times. The world is made under a mod set
    // with five blocks in front of the kit, and reopened without them and
    // with the kit registering oak before stone: every runtime id moves, and
    // the two materials change places. The cut must still be stone where it
    // was stone and oak where it was oak — in the hand, in the world once
    // placed, and in the units breaking it gives back.
    let world = scratch("renumbered-world");
    let seed = Identity::generate().expect("identity").seed();
    let (names, stone_first, oak_first);

    {
        let server = start_at(mods("renumbered-mods-1", true, false), world.clone());
        (names, stone_first, oak_first) = block_on(async {
            let mut bot = join_as(&server, "Keeper", Identity::from_seed(&seed)).await;
            let (stone, oak) = (wire_id(&bot, "kit:stone"), wire_id(&bot, "kit:oak"));
            let cut = make(&mut bot, stone, oak, &painted(stone, oak)).await;
            assert_eq!(
                cut.material, stone,
                "stone is the lowest here, world and session"
            );
            let names = names_of(&bot, &cut.cells);
            bot.disconnect().await;
            (names, stone, oak)
        });
        assert!(server.stop(), "the world did not flush cleanly");
    }

    let server = start_at(mods("renumbered-mods-2", false, true), world);
    block_on(async {
        let mut bot = join_as(&server, "Keeper", Identity::from_seed(&seed)).await;
        let (stone, oak) = (wire_id(&bot, "kit:stone"), wire_id(&bot, "kit:oak"));
        assert_eq!(
            (stone, oak),
            (stone_first, oak_first),
            "the world's ids are the world's, whatever the mod set"
        );
        // **Prove the ids moved**, so nothing below passes by coincidence:
        // the session numbers oak below stone, and neither as the world does.
        let (runtime_oak, runtime_stone) = runtime_ids(&mut bot).await;
        assert!(
            runtime_oak < runtime_stone,
            "the session did not put oak first: oak {runtime_oak}, stone {runtime_stone}"
        );
        assert_ne!(
            runtime_oak,
            i32::from(oak),
            "the session's id for oak is the world's, so nothing was renumbered"
        );

        let held = inventory_where(&mut bot, "the cut did not come back", |stacks| {
            carried_cut(stacks).is_some()
        })
        .await;
        let cut = carried_cut(&held).expect("checked above");
        assert_eq!(
            names_of(&bot, &cut.cells),
            names,
            "the cut is not made of what it was made of"
        );
        assert_eq!(cut.units, 12);
        // §9.1 rule 2: the material is the lowest id among the cells IN THIS
        // SESSION, derived again on the way in — oak now, though the world
        // numbers stone lower.
        assert_eq!(
            cut.material, oak,
            "the cut's material was not derived again in this session's ids"
        );

        let placed = place(&mut bot, &cut).await;
        let placed: Vec<u16> = placed.iter().map(|cell| cell.0).collect();
        assert_eq!(
            names_of(&bot, &placed),
            names,
            "the placed block is not made of what the cut was made of"
        );
        break_it(&mut bot, |stacks| {
            loose(stacks, stone) == 27 && loose(stacks, oak) == 27
        })
        .await;
        bot.disconnect().await;
    });
    assert!(server.stop());
}
