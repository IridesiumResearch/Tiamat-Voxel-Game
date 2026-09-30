// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! A sky set on one domain while the world runs, over a real server and real
//! bots: Magic's E-M2, a woven world with a dawn of its own.
//!
//! What this drives is the four things a mod relies on: a player inside is
//! told at once, a player arriving later is told on arrival, `nil` gives the
//! template's sky back, and the sky is kept with the instance across a restart
//! and goes when `destroy_domain` removes it.

use std::path::PathBuf;
use std::time::Duration;

use bot::Bot;
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::interest::ViewDistance;
use tiamat_core::proto::ServerMessage;
use tiamat_server::{ServerHandle, Settings};

const MATERIALS: [&str; 1] = ["test:stone"];

/// The stars each sky in the fixture declares, which is how a test reads
/// which sky a table is.
const HOME: f32 = 1.0;
const TEMPLATE: f32 = 0.5;
const SET: f32 = 0.8;
const AT_CREATION: f32 = 0.25;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamat-domain-sky").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

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
         } }\n\
         game.register_domain{ id = \"body\", instanced = true, generator = function(buf, pos)\n\
         \x20   buf:fill_below_heightmap(game.flat_heightmap(0), ground)\n\
         end }\n\
         game.register_sky{ domain = \"space:body\", day_length_ticks = 24000, keyframes = {\n\
         \x20   { time = 0.0, sky = {0, 0, 0}, sun = {0, 0, 0}, intensity = 0.0, stars = 0.5 },\n\
         } }\n\
         local function sky(stars)\n\
         \x20   return { keyframes = {\n\
         \x20       { time = 0.0, sky = {0.4, 0.1, 0.1}, sun = {1, 0.5, 0.5}, intensity = 1.0, stars = stars },\n\
         \x20   }, cave_fog = {0.3, 0.0, 0.0} }\n\
         end\n\
         game.register_on_chat(function(event)\n\
         \x20   local verb, key = event.text:match(\"^(%a+) (%d+)$\")\n\
         \x20   if verb == nil then return end\n\
         \x20   local id = 'space:body/' .. key\n\
         \x20   local body = game.player_entity(event.player)\n\
         \x20   if verb == 'enter' then\n\
         \x20       game.transfer_entity(body, game.create_domain('space:body', key), { x = 8, y = 4, z = 8 })\n\
         \x20   elseif verb == 'enterwith' then\n\
         \x20       local sky = sky(0.25)\n\
         \x20       game.transfer_entity(body, game.create_domain('space:body', key, { sky = sky }), { x = 8, y = 4, z = 8 })\n\
         \x20   elseif verb == 'sky' then\n\
         \x20       if not game.set_domain_sky(id, sky(0.8)) then game.log('NO DOMAIN ' .. id) end\n\
         \x20   elseif verb == 'clear' then\n\
         \x20       game.set_domain_sky(id, nil)\n\
         \x20   elseif verb == 'home' then\n\
         \x20       game.transfer_entity(body, 'overworld', { x = 8, y = 4, z = 8 })\n\
         \x20   elseif verb == 'destroy' then\n\
         \x20       game.destroy_domain(id)\n\
         \x20   end\n\
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

/// The stars of the newest sky table this bot has been sent, and how many
/// tables it has been sent in all.
fn latest_sky(bot: &Bot) -> Option<(f32, usize)> {
    let tables: Vec<f32> = bot
        .received()
        .into_iter()
        .filter_map(|message| match message {
            ServerMessage::SkyTable { keyframes, .. } => keyframes.first().map(|frame| frame.stars),
            _ => None,
        })
        .collect();
    tables.last().map(|stars| (*stars, tables.len()))
}

/// The newest cave fog this bot was sent.
fn latest_cave_fog(bot: &Bot) -> Option<[f32; 3]> {
    bot.received()
        .into_iter()
        .rev()
        .find_map(|message| match message {
            ServerMessage::SkyTable { cave_fog, .. } => Some(cave_fog),
            _ => None,
        })
}

/// Pumps the connection until the newest sky table has these stars.
async fn wait_for_sky(bot: &mut Bot, stars: f32, what: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if latest_sky(bot).is_some_and(|(seen, _)| (seen - stars).abs() < f32::EPSILON) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what}: wanted the sky with {stars} stars, the newest table has {:?}",
            latest_sky(bot)
        );
        let _ = tokio::time::timeout(Duration::from_millis(60), bot.recv()).await;
    }
}

/// Says something in chat and waits to be in the domain it names, having
/// been sent its sky after arriving.
async fn enter(bot: &mut Bot, command: &str, domain: &str) {
    bot.chat(command).await.expect("chat");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let received = bot.received();
        if let Some(at) = received.iter().rposition(
            |message| matches!(message, ServerMessage::DomainChanged { domain: d } if d == domain),
        ) && received[at..]
            .iter()
            .any(|message| matches!(message, ServerMessage::SkyTable { .. }))
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "`{command}` never brought the player to {domain}"
        );
        let _ = tokio::time::timeout(Duration::from_millis(60), bot.recv()).await;
    }
}

async fn settle_for(bot: &mut Bot, ticks: u64) {
    for _ in 0..ticks {
        let _ = tokio::time::timeout(Duration::from_millis(60), bot.recv()).await;
    }
}

#[test]
fn a_sky_set_on_an_instance_reaches_those_inside_and_those_who_come_later() {
    let server = start_at(scratch("reach-world"), write_mod("reach"));
    block_on(async {
        let mut inside = join(&server, "Weaver").await;
        settle_for(&mut inside, 20).await;
        assert_eq!(latest_sky(&inside).map(|(stars, _)| stars), Some(HOME));

        // The template's sky, until somebody says otherwise.
        enter(&mut inside, "enter 1", "space:body/1").await;
        wait_for_sky(&mut inside, TEMPLATE, "the template's sky").await;

        // A change reaches the player already inside.
        inside.chat("sky 1").await.expect("chat");
        wait_for_sky(&mut inside, SET, "a sky set while inside").await;
        assert_eq!(latest_cave_fog(&inside), Some([0.3, 0.0, 0.0]));

        // A player arriving later is given it on arrival.
        let mut later = join(&server, "Latecomer").await;
        settle_for(&mut later, 20).await;
        assert_eq!(
            latest_sky(&later).map(|(stars, _)| stars),
            Some(HOME),
            "the overworld's sky changed with the instance's"
        );
        enter(&mut later, "enter 1", "space:body/1").await;
        wait_for_sky(&mut later, SET, "a player arriving after the change").await;

        // Another instance of the same template is untouched.
        enter(&mut later, "enter 2", "space:body/2").await;
        wait_for_sky(&mut later, TEMPLATE, "a sibling instance").await;

        // `nil` returns the template's sky to everybody inside.
        inside.chat("clear 1").await.expect("chat");
        wait_for_sky(&mut inside, TEMPLATE, "a sky cleared with nil").await;

        inside.disconnect().await;
        later.disconnect().await;
    });
    assert!(server.stop());
}

#[test]
fn a_sky_survives_a_restart_and_is_gone_with_its_instance() {
    let world = scratch("keep-world");
    let mods = write_mod("keep");

    let first = start_at(world.clone(), mods.clone());
    block_on(async {
        let mut bot = join(&first, "Weaver").await;
        settle_for(&mut bot, 20).await;
        enter(&mut bot, "enterwith 7", "space:body/7").await;
        wait_for_sky(&mut bot, AT_CREATION, "a sky given at creation").await;
        bot.chat("sky 7").await.expect("chat");
        wait_for_sky(&mut bot, SET, "a sky set after").await;
        // Let the tick write it out.
        settle_for(&mut bot, 20).await;
        bot.disconnect().await;
    });
    assert!(first.stop(), "the world should close cleanly");

    let second = start_at(world, mods);
    block_on(async {
        let mut bot = join(&second, "Returner").await;
        settle_for(&mut bot, 20).await;
        // Making it again asks for no sky, and moves nothing, so what comes
        // back is what the world file kept.
        enter(&mut bot, "enter 7", "space:body/7").await;
        wait_for_sky(&mut bot, SET, "the sky after a restart").await;

        // Destroyed, it takes the sky with it: made again it is plain.
        bot.chat("home 0").await.expect("chat");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while latest_sky(&bot).is_some_and(|(stars, _)| (stars - HOME).abs() > f32::EPSILON) {
            assert!(tokio::time::Instant::now() < deadline, "never went home");
            let _ = tokio::time::timeout(Duration::from_millis(60), bot.recv()).await;
        }
        settle_for(&mut bot, 10).await;
        bot.chat("destroy 7").await.expect("chat");
        settle_for(&mut bot, 20).await;
        enter(&mut bot, "enter 7", "space:body/7").await;
        wait_for_sky(&mut bot, TEMPLATE, "an instance made again after a destroy").await;
        bot.disconnect().await;
    });
    assert!(second.stop());
}
