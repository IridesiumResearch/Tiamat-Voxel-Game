// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! A world option chosen when a world is made shapes its terrain, and is kept
//! for the life of the world whatever a later start asks for.
//!
//! **Asked for from the window: a start-screen setting that makes the whole
//! world one biome.** The mod declares the choice in `mod.toml`, the screen
//! hands it to the server on creation, the server stores it with the world and
//! puts it on `game` in every VM — the tick's and the generation workers' — so
//! a generator can read it. This drives all of that through a real server and
//! reads the answer off the wire as terrain.

use std::path::{Path, PathBuf};
use std::time::Duration;

use bot::Bot;
use tiamat_core::BlockPos;
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::interest::ViewDistance;
use tiamat_server::{ServerHandle, Settings};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamat-world-options").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// A mod whose whole world is one material, chosen by a world option.
fn write_mod(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("biomes");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"biomes\"\nname = \"Biomes\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n\n\
         [[world_option]]\n\
         id = \"biome\"\n\
         name = \"Biome\"\n\
         options = [\"grass\", \"sand\", \"snow\"]\n\
         default = 1\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        "local grass = game.register_block{ id = \"grass\" }\n\
         local sand = game.register_block{ id = \"sand\" }\n\
         local snow = game.register_block{ id = \"snow\" }\n\
         -- Read ONCE at load, as the guide says: it is fixed for the world.\n\
         local chosen = game.world_option(\"biomes:biome\")\n\
         local ground = ({ grass = grass, sand = sand, snow = snow })[chosen] or grass\n\
         game.register_on_generate(function(buf, pos)\n\
         \x20   buf:fill_below_heightmap(game.flat_heightmap(4), ground)\n\
         end)\n",
    )
    .expect("script");
    root
}

fn start(world: &Path, mods: &Path, chosen: &[(&str, &str)]) -> ServerHandle {
    ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: world.to_path_buf(),
        identity_path: None,
        max_players: 2,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(mods.to_path_buf()),
        enabled_mods: None,
        seed: Some(5),
        rcon: None,
        materials: Vec::new(),
        world_options: chosen
            .iter()
            .map(|(id, value)| ((*id).to_owned(), (*value).to_owned()))
            .collect(),
    })
    .expect("start")
}

/// What the ground at the origin is made of, by name, as a client sees it.
fn ground_material(server: &ServerHandle, visitor: &str) -> String {
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
            // A name of its own per visit: a name is bound to the UUID that
            // first claimed it in a world (charter rule 13), and every visit
            // here is a fresh identity.
            bot.join(visitor).await.expect("join");
            let origin = tiamat_core::ChunkPos::new(0, 0, 0);
            let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
            while !bot.chunks_received().contains(&origin) && tokio::time::Instant::now() < deadline
            {
                let _ = bot.collect_chunks(16, Duration::from_millis(500)).await;
            }
            // An id map built from the table the server sent, in its order,
            // the way every other streamed-chunk test builds one.
            let table = bot.material_table().expect("material table");
            let mut registry = tiamat_core::Registry::new();
            for entry in &table {
                // The engine's own two are in a fresh registry already.
                if entry.name.starts_with("engine:") {
                    continue;
                }
                registry.register(&entry.name).expect("register");
            }
            let db = tiamat_core::WorldDb::open_in_memory(&mut registry).expect("id map");
            let chunk = bot
                .decode_chunk(origin, db.materials())
                .expect("the origin chunk");
            let id = chunk
                .get_block(BlockPos::new(8, 2, 8))
                .expect("in chunk")
                .subnode(0);
            let name = table
                .iter()
                .find(|entry| entry.id == id.get())
                .map(|entry| entry.name.clone())
                .unwrap_or_else(|| format!("#{}", id.get()));
            bot.disconnect().await;
            name
        })
}

#[test]
fn a_world_option_chosen_at_creation_shapes_the_terrain_and_is_kept_for_the_life_of_the_world() {
    let mods = write_mod("mods");
    let world = scratch("world");

    // Made as sand.
    let server = start(&world, &mods, &[("biomes:biome", "sand")]);
    assert_eq!(
        ground_material(&server, "Settler"),
        "biomes:sand",
        "the choice did not reach the generator"
    );
    assert!(server.stop());

    // **Reopened asking for snow: still sand.** The choice belongs to the
    // world, exactly as the seed does — a start that could change it would
    // generate snow beyond the explored edge of a sand world.
    let server = start(&world, &mods, &[("biomes:biome", "snow")]);
    assert_eq!(
        ground_material(&server, "Returner"),
        "biomes:sand",
        "an existing world took a new choice, which would change terrain beyond what has been \
         explored"
    );
    assert!(server.stop());

    // And a world made choosing nothing gets the declared default.
    let plain = scratch("plain");
    let server = start(&plain, &mods, &[]);
    assert_eq!(
        ground_material(&server, "Newcomer"),
        "biomes:grass",
        "the default did not apply"
    );
    assert!(server.stop());
}
