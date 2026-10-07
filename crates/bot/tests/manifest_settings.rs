// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! A setting declared as `[[setting]]` in `mod.toml` (UI ask 20, part 2), over
//! a real server.
//!
//! The start screen runs no Lua, so a mod's settings had to be declarable in
//! its manifest. This proves the manifest route is the SAME setting as
//! `game.register_setting` from the wire's point of view: it is in the
//! `ModSettings` table with the right kind, options and zero-based default, the
//! server accepts a `SetSetting` for it, and `game.setting` answers the
//! default before and the player's value after. The mod has NO Lua
//! registration at all.

use std::path::PathBuf;
use std::time::Duration;

use bot::Bot;
use tiamat_core::BlockPos;
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::interest::ViewDistance;
use tiamat_core::proto::SettingKind;
use tiamat_server::{ServerHandle, Settings};

const PATIENCE: Duration = Duration::from_secs(30);

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("tiamat-manifest-settings")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// A mod that declares two settings in its manifest only, and reports what
/// `game.setting` answers as blocks (the only way anything reaches a test).
fn write_declared(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("declared");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"declared\"\nname = \"Declared\"\nversion = \"0.1.0\"\n\
         license = \"GPL-3.0-only\"\n\n\
         [[setting]]\nid = \"marker\"\nname = \"Place a marker\"\ndefault = 0\n\n\
         [[setting]]\nid = \"size\"\nname = \"Marker size\"\n\
         options = [\"small\", \"large\"]\ndefault = 1\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        "local ground = game.register_block{ id = \"ground\" }\n\
         local yes = game.register_block{ id = \"yes\" }\n\
         local no = game.register_block{ id = \"no\" }\n\
         game.register_on_generate(function(buf, pos)\n\
         \x20   buf:fill_below_heightmap(game.flat_heightmap(0), ground)\n\
         end)\n\
         local who = nil\n\
         game.register_on_player_join(function(event) who = event.player end)\n\
         game.register_on_tick(function()\n\
         \x20   if who == nil then return end\n\
         \x20   local on = game.setting(who, \"declared:marker\")\n\
         \x20   local size = game.setting(who, \"declared:size\")\n\
         \x20   game.set_block({ x = 2, y = 9, z = 2 }, on and \"declared:yes\" or \"declared:no\")\n\
         \x20   game.set_block({ x = 3, y = 9, z = 2 }, size == \"large\" and \"declared:yes\" or \"declared:no\")\n\
         end)\n",
    )
    .expect("script");
    root
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
        seed: Some(11),
        rcon: None,
        materials: Vec::new(),
        world_options: Vec::new(),
    })
    .expect("start")
}

#[test]
fn a_manifest_setting_is_offered_accepted_and_answered_like_a_registered_one() {
    let server = start(write_declared("mods"), scratch("world"));
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(async {
            let mut bot = Bot::connect(
                server.local_addr(),
                Identity::generate().expect("identity"),
                server.cert_fingerprint(),
            )
            .await
            .expect("connect");
            bot.join("Chooser").await.expect("join");

            let table = bot.material_table().expect("material table");
            let id_of = |name: &str| {
                table
                    .iter()
                    .find(|entry| entry.name == name)
                    .map(|entry| entry.id)
                    .unwrap_or_else(|| panic!("the mod registers {name}"))
            };

            // In the table, with no Lua registration behind it.
            let settings = bot.mod_settings().expect("the server sends its settings");
            assert_eq!(settings.len(), 2, "{settings:?}");
            assert_eq!(settings[0].id, "declared:marker");
            assert_eq!(settings[0].mod_id, "declared");
            assert_eq!(settings[0].kind, SettingKind::Toggle);
            assert_eq!(settings[0].default, 0);
            assert_eq!(settings[1].id, "declared:size");
            assert_eq!(settings[1].kind, SettingKind::Choice);
            assert_eq!(settings[1].options, ["small", "large"]);
            assert_eq!(
                settings[1].default, 0,
                "the manifest's one-based `default = 1` is the zero-based index 0"
            );

            // The control: before any answer, game.setting answers the
            // defaults: marker off, size "small".
            bot.expect_block(BlockPos::new(2, 9, 2), id_of("declared:no"), PATIENCE)
                .await
                .expect("the default toggle is off");
            bot.expect_block(BlockPos::new(3, 9, 2), id_of("declared:no"), PATIENCE)
                .await
                .expect("the default choice is the first option");

            // Answered: accepted by the server, and read back by the mod.
            bot.set_setting("declared:marker", 1).await.expect("answer");
            bot.set_setting("declared:size", 1).await.expect("answer");
            bot.expect_block(BlockPos::new(2, 9, 2), id_of("declared:yes"), PATIENCE)
                .await
                .expect("the mod should read the answered toggle");
            bot.expect_block(BlockPos::new(3, 9, 2), id_of("declared:yes"), PATIENCE)
                .await
                .expect("the mod should read the answered choice");
        });
}
