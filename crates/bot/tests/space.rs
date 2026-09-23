// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Visiting a star: Task 15c's round trip, through a real server and a real
//! bot, with the mod's half written as a mod would write it.
//!
//! The engine's four asks, each exercised on the wire: a sky per domain with
//! stars in it, sent for the domain a player arrives in; a domain instance
//! made at a place in the universe, and that place surviving a restart; the
//! catalog a mod reads being the catalog the engine draws; and the sky table
//! carrying where it is seen from.

use std::path::PathBuf;
use std::time::Duration;

use bot::Bot;
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::interest::ViewDistance;
use tiamat_core::proto::ServerMessage;
use tiamat_server::{ServerHandle, Settings};

const MATERIALS: [&str; 1] = ["test:stone"];
const SEED: u64 = 3;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamat-space").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// The mod: a home sky with stars at night, a template for bodies with a
/// sky of its own, and a chat command that makes a body AT a star from the
/// catalog and moves the player there. `revisit` makes the same body with no
/// position at all, which is how the test tells a remembered place from one
/// the command supplied again.
fn write_mod(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("space");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"space\"\nname = \"Space\"\nversion = \"0.1.0\"\n\
         license = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        "local ground = game.register_block{ id = \"ground\" }\n\
         game.register_on_generate(function(buf, pos)\n\
         \x20   buf:fill_below_heightmap(game.flat_heightmap(0), ground)\n\
         end)\n\
         game.register_tool{ id = \"hand\", brush = \"block\", speed_multiplier = 1.0, default = true }\n\
         game.register_sky{ day_length_ticks = 24000, keyframes = {\n\
         \x20   { time = 0.0, sky = {0, 0, 0.05}, sun = {0.2, 0.2, 0.4}, intensity = 0.05, stars = 1.0 },\n\
         \x20   { time = 0.5, sky = {0.5, 0.7, 1.0}, sun = {1, 1, 1}, intensity = 1.0 },\n\
         } }\n\
         game.register_domain{ id = \"body\", instanced = true, generator = function(buf, pos)\n\
         \x20   buf:fill_below_heightmap(game.flat_heightmap(0), ground)\n\
         end }\n\
         game.register_sky{ domain = \"space:body\", day_length_ticks = 24000, keyframes = {\n\
         \x20   { time = 0.0, sky = {0, 0, 0}, sun = {0, 0, 0}, intensity = 0.0, stars = 0.5 },\n\
         } }\n\
         game.register_on_chat(function(event)\n\
         \x20   local verb, id = event.text:match(\"^(%a+) (%d+)$\")\n\
         \x20   if verb ~= \"visit\" and verb ~= \"revisit\" then return end\n\
         \x20   id = tonumber(id)\n\
         \x20   local domain\n\
         \x20   if verb == \"revisit\" then\n\
         \x20       domain = game.create_domain(\"space:body\", tostring(id))\n\
         \x20   else\n\
         \x20       for _, star in ipairs(game.stars()) do\n\
         \x20           if star.id == id then\n\
         \x20               domain = game.create_domain(\"space:body\", tostring(id),\n\
         \x20                   { position = { x = star.x, y = star.y, z = star.z } })\n\
         \x20           end\n\
         \x20       end\n\
         \x20   end\n\
         \x20   if domain == nil then game.log(\"no star \" .. id) return false end\n\
         \x20   game.transfer_entity(game.player_entity(event.player), domain, { x = 8, y = 4, z = 8 })\n\
         \x20   return false\n\
         end)\n",
    )
    .expect("script");
    root
}

fn start_at(world: PathBuf, mods: PathBuf) -> ServerHandle {
    ServerHandle::start(&Settings {
        world_options: Vec::new(),
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: world,
        identity_path: None,
        max_players: 4,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(mods),
        enabled_mods: None,
        seed: Some(SEED),
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

async fn settle_for(bot: &mut Bot, ticks: u64) {
    for _ in 0..ticks {
        let _ = tokio::time::timeout(Duration::from_millis(60), bot.recv()).await;
    }
}

/// The domain the bot was last moved to, and the sky table that followed it.
fn arrival(bot: &Bot) -> Option<(String, u32, Vec<tiamat_core::proto::SkyFrame>, [i64; 3])> {
    let received = bot.received();
    let moved = received
        .iter()
        .rposition(|message| matches!(message, ServerMessage::DomainChanged { .. }))?;
    let ServerMessage::DomainChanged { domain } = &received[moved] else {
        return None;
    };
    received[moved..].iter().find_map(|message| match message {
        ServerMessage::SkyTable {
            day_length_ticks,
            keyframes,
            observer,
        } => Some((
            domain.clone(),
            *day_length_ticks,
            keyframes.clone(),
            *observer,
        )),
        _ => None,
    })
}

/// The first sky table the join sent: the overworld's.
fn home_sky(bot: &Bot) -> (Vec<tiamat_core::proto::SkyFrame>, [i64; 3]) {
    bot.received()
        .into_iter()
        .find_map(|message| match message {
            ServerMessage::SkyTable {
                keyframes,
                observer,
                ..
            } => Some((keyframes, observer)),
            _ => None,
        })
        .expect("the join flow includes a sky table")
}

async fn wait_for_arrival(
    bot: &mut Bot,
    what: &str,
) -> (String, u32, Vec<tiamat_core::proto::SkyFrame>, [i64; 3]) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(arrived) = arrival(bot) {
            return arrived;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what}: the player was never moved, or the sky never followed"
        );
        let _ = tokio::time::timeout(Duration::from_millis(60), bot.recv()).await;
    }
}

#[test]
fn a_visited_star_has_its_own_sky_seen_from_where_it_is_and_is_still_there_after_a_restart() {
    let world = scratch("visit-world");
    let mods = write_mod("visit");
    // The star the mod will read out of `game.stars()`: the engine's own
    // catalog for the seed, so the two are one derivation.
    let catalog = tiamat_core::sky::star_catalog(SEED);
    let star = catalog[300];
    let home = tiamat_core::sky::world_position(SEED);
    let at = |position: tiamat_core::sky::UniversalPos| [position.x, position.y, position.z];

    let first = start_at(world.clone(), mods.clone());
    block_on(async {
        let mut bot = join(&first, "Voyager").await;
        settle_for(&mut bot, 20).await;

        // Home: the sky for every domain not named, seen from the world's
        // own place in the universe, with stars at night.
        let (keyframes, observer) = home_sky(&bot);
        assert_eq!(observer, at(home), "the overworld's sky is seen from home");
        assert!((keyframes[0].stars - 1.0).abs() < f32::EPSILON);
        assert!((keyframes[1].stars - 0.0).abs() < f32::EPSILON);

        bot.chat(&format!("visit {}", star.id)).await.expect("chat");
        let (domain, day, keyframes, observer) = wait_for_arrival(&mut bot, "visit").await;
        assert_eq!(domain, format!("space:body/{}", star.id));
        // The body's sky: the template's, inherited by the instance, with the
        // world's one day, seen from the star.
        assert_eq!(
            day, 24_000,
            "one clock: the world's day, not the domain sky's own"
        );
        assert_eq!(keyframes.len(), 1);
        assert!((keyframes[0].stars - 0.5).abs() < f32::EPSILON);
        assert_eq!(
            observer,
            at(star.position),
            "the body's sky is not seen from the star the mod made it at"
        );
        bot.disconnect().await;
    });
    assert!(first.stop(), "the world should close cleanly");

    // Restarted, and the body made again WITHOUT a position: the instance
    // and where it sits both came back from the world file, or the sky would
    // be seen from home.
    let second = start_at(world, mods);
    block_on(async {
        let mut bot = join(&second, "Visitor").await;
        settle_for(&mut bot, 20).await;
        bot.chat(&format!("revisit {}", star.id))
            .await
            .expect("chat");
        let (domain, _, keyframes, observer) = wait_for_arrival(&mut bot, "revisit").await;
        assert_eq!(domain, format!("space:body/{}", star.id));
        assert!((keyframes[0].stars - 0.5).abs() < f32::EPSILON);
        assert_eq!(
            observer,
            at(star.position),
            "after a restart the body is seen from home: its place did not survive"
        );
        bot.disconnect().await;
    });
    assert!(second.stop());
}
