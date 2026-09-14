// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! A generated sea, streamed: every wet chunk arrives with its water.
//!
//! Reported from the window: walls rendering between the fluid chunks of an
//! ocean. A chunk drawn without its fluid layer is exactly that — a dry square
//! with a wall of water from each wet neighbour — and a served chunk's layer
//! used to go out on the BROADCAST, a bounded channel a client that falls
//! behind loses messages from, where nothing would ever re-send a still sea.
//! It now travels on the requester's own reply, immediately before the chunk.
//!
//! **A guard, not a reproduction, and said so.** Flooding the broadcast while a
//! client stopped reading made it fall behind 140–183 times a run, and the old
//! code still lost no chunk's layer in that fixture: the serves happen while the
//! connection is flowing, so what the lag dropped was the flood. What this holds
//! is the ordering the reply makes certain — the layer first, then the chunk.

use std::path::PathBuf;
use std::time::Duration;

use bot::Bot;
use tiamot_core::identity::{Allowlist, Identity};
use tiamot_core::interest::ViewDistance;
use tiamot_core::proto::ServerMessage;
use tiamot_server::{ServerHandle, Settings};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamot-sea").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// A sea with its surface at y = 8 over a floor at y = -20.
fn write_sea(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("sea");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"sea\"\nname = \"Sea\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        "local sand = game.register_block{ id = \"sand\" }\n\
         game.register_block{ id = \"water\" }\n\
         game.register_fluid{ id = \"water\", material = \"sea:water\" }\n\
         game.register_on_generate(function(buf, pos)\n\
         \x20   buf:fill_below_heightmap(game.flat_heightmap(-20), sand)\n\
         \x20   buf:fill_fluid_below(8, \"sea:water\")\n\
         end)\n",
    )
    .expect("script");
    root
}

#[test]
fn every_chunk_of_a_sea_arrives_after_its_own_water() {
    let server = ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: scratch("sea-world"),
        identity_path: None,
        max_players: 2,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance {
            horizontal: 3,
            vertical: 2,
        },
        mods_path: Some(write_sea("sea-mods")),
        enabled_mods: None,
        seed: Some(11),
        rcon: None,
        materials: Vec::new(),
    })
    .expect("start");

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
            bot.join("Diver").await.expect("join");
            loop {
                let batch = bot
                    .collect_chunks(64, Duration::from_secs(2))
                    .await
                    .expect("collect");
                if batch.is_empty() {
                    break;
                }
            }

            // Every block of y = -1 is under the surface and over the floor,
            // and y = 0 is wet below 8, so each layer is non-empty.
            let received = bot.received();
            let mut checked = 0;
            for (index, message) in received.iter().enumerate() {
                let ServerMessage::ChunkData { pos, .. } = message else {
                    continue;
                };
                if !(-1..=0).contains(&pos.y) {
                    continue;
                }
                checked += 1;
                let layer = received[..index]
                    .iter()
                    .rev()
                    .find_map(|earlier| match earlier {
                        ServerMessage::ChunkFluid { pos: at, fluid } if at == pos => {
                            tiamot_core::fluid::codec::decode(fluid).ok()
                        }
                        _ => None,
                    });
                let Some(layer) = layer else {
                    panic!("the chunk at {pos:?} arrived with no water before it");
                };
                assert!(
                    !layer.is_empty(),
                    "the chunk at {pos:?} came with an empty sea"
                );
            }
            assert!(
                checked >= 9,
                "only {checked} chunks of sea were sent to check"
            );
            bot.disconnect().await;
        });
    assert!(server.stop());
}
