// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! `game.set_sky_modifier` over a real server — weather ask W1. The seam is
//! installed by the server and nothing else, so this is the test that says it
//! is installed at all.

use std::path::PathBuf;
use std::time::Duration;

use bot::Bot;
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::interest::ViewDistance;
use tiamat_server::{ServerHandle, Settings};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamat-atmosphere").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// A mod that darkens a player's sky when they join, sets the same thing
/// again every tick (which must cost nothing), and clears it a second later.
fn write_storm(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("storm");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"storm\"\nname = \"Storm\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        r#"
local ground = game.register_block{ id = "ground" }
game.register_on_generate(function(buf, pos)
    buf:fill_below_heightmap(game.flat_heightmap(0), ground)
end)
local since = {}
game.register_on_player_join(function(event)
    since[event.player] = 0
    -- Lightning beside them, and a strike far out of sight (W3).
    game.flash{ pos = { x = 30, y = 80, z = 0 }, radius = 256, intensity = 1.5,
                colour = { 0.9, 0.92, 1.0 }, attack_ticks = 1, decay_ticks = 6 }
    game.flash{ pos = { x = 5000, y = 80, z = 0 }, radius = 256, intensity = 0.25 }
end)
game.register_on_tick(function()
    for player, ticks in pairs(since) do
        since[player] = ticks + 1
        if ticks < 20 then
            game.set_sky_modifier(player, { intensity = 0.55, sky = { 0.55, 0.58, 0.62 },
                                            sky_mix = 0.7, fog_distance = 0.6, ease_ticks = 400 })
            game.set_precipitation(player, { rate = 900, size = 0.06, velocity = { x = 3, y = -22, z = 0 },
                                             area = { x = 16, y = 3, z = 16 }, above = 18, ease_ticks = 200 })
        elseif ticks == 20 then
            game.set_sky_modifier(player, nil)
            game.set_precipitation(player, nil)
        end
    end
end)
"#,
    )
    .expect("script");
    root
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(future)
}

/// A storm that flashes for each player who joins, to that player alone.
fn write_aimed_storm(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("storm");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"storm\"\nname = \"Storm\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        r#"
local ground = game.register_block{ id = "ground" }
game.register_on_generate(function(buf, pos)
    buf:fill_below_heightmap(game.flat_heightmap(0), ground)
end)
game.register_on_player_join(function(event)
    -- Beside spawn, so everybody is in range; addressed to the one who
    -- just arrived (W28), so nobody else sees it.
    game.flash{ pos = { x = 0, y = 80, z = 0 }, radius = 256, intensity = 1.0, player = event.player }
end)
"#,
    )
    .expect("script");
    root
}

/// A storm that draws a bolt for each player who joins, to that player
/// alone — weather ask W26, addressed as W28 addresses a flash.
fn write_aimed_bolts(name: &str) -> PathBuf {
    write_mod(
        name,
        r#"
game.register_on_player_join(function(event)
    -- Straight down on spawn, so everybody is in range of its top; addressed
    -- to the one who just arrived, so nobody else sees it.
    game.lightning{ from = { x = 0, y = 300, z = 0 }, to = { x = 0, y = 0, z = 0 },
                    radius = 512, player = event.player }
end)
"#,
    )
}

/// A storm that strikes twice over everybody once two players are in: one
/// bolt with a seed past 2^63, and one the engine picks a seed for.
fn write_shared_bolts(name: &str, seed: u64) -> PathBuf {
    write_mod(
        name,
        &format!(
            r#"
local joined = 0
game.register_on_player_join(function(event)
    joined = joined + 1
    if joined == 2 then
        game.lightning{{ from = {{ x = 30, y = 300, z = 0 }}, to = {{ x = 40, y = 0, z = 10 }},
                        seed = {seed}, branches = 8, radius = 1024 }}
        game.lightning{{ from = {{ x = 30, y = 300, z = 0 }}, to = {{ x = 40, y = 0, z = 10 }},
                        radius = 1024 }}
    end
end)
"#,
            // A seed past 2^63 is a negative integer in Lua; by its bits it is
            // the seed.
            seed = seed.cast_signed()
        ),
    )
}

/// A mod named `storm` with a flat floor and `script` after it.
fn write_mod(name: &str, script: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("storm");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"storm\"\nname = \"Storm\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        format!(
            r#"
local ground = game.register_block{{ id = "ground" }}
game.register_on_generate(function(buf, pos)
    buf:fill_below_heightmap(game.flat_heightmap(0), ground)
end)
{script}"#
        ),
    )
    .expect("script");
    root
}

/// A server over the mods in `mods`, with room for four.
fn serve(mods: &std::path::Path, world: &str) -> ServerHandle {
    ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: scratch(world),
        identity_path: None,
        max_players: 4,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(mods.to_path_buf()),
        enabled_mods: bot::fixture::enabled_mods_for(mods).expect("the mod's manifest"),
        seed: Some(5),
        rcon: None,
        materials: Vec::new(),
        world_options: Vec::new(),
    })
    .expect("start")
}

/// A bot joined to `server` as `name`.
async fn joined(server: &ServerHandle, name: &str) -> Bot {
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

/// Receives on `bot` until it holds `count` bolts or ten seconds pass,
/// keeping `other` drained meanwhile.
async fn await_bolts(bot: &mut Bot, other: Option<&mut Bot>, count: usize) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut other = other;
    while tokio::time::Instant::now() < deadline && bot.lightning_received().len() < count {
        let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
        if let Some(other) = other.as_deref_mut() {
            let _ = tokio::time::timeout(Duration::from_millis(20), other.recv()).await;
        }
    }
}

#[test]
fn a_bolt_addressed_to_one_player_reaches_nobody_else() {
    // Weather ask W26, addressed as W28 addresses a flash: two players in
    // range of every bolt, each bolt addressed to one of them, and the
    // other's client receives nothing.
    let mods = write_aimed_bolts("aimed-bolts");
    let server = serve(&mods, "aimed-bolts-world");

    block_on(async {
        let mut first = joined(&server, "First").await;
        await_bolts(&mut first, None, 1).await;
        assert_eq!(
            first.lightning_received().len(),
            1,
            "the bolt addressed to the first"
        );

        let mut second = joined(&server, "Second").await;
        await_bolts(&mut second, Some(&mut first), 1).await;
        assert_eq!(
            second.lightning_received().len(),
            1,
            "the bolt addressed to the second"
        );
        // Twenty server ticks more for anything wrongly sent to the first,
        // in real time, for the reason the flash's test gives.
        first.sleep_ticks(20).await;
        for _ in 0..10 {
            let _ = tokio::time::timeout(Duration::from_millis(20), first.recv()).await;
        }
        assert_eq!(
            first.lightning_received().len(),
            1,
            "a bolt addressed to the second player reached the first"
        );
    });

    assert!(server.stop());
}

#[test]
fn two_players_given_the_same_bolt_build_the_same_path() {
    // The gate's middle clause: "two clients given the same seed draw the
    // same path." The path never travels, so this is the whole chain — the
    // mod's seed through Lua by its bits, the server's relay, the wire, and
    // each client building the path on its own — ending in two hashes that
    // must agree. The second bolt names no seed: the engine picks one, and
    // everyone watching must still be handed the same.
    let seed = 16_099_289_709_293_836_018_u64;
    let mods = write_shared_bolts("shared-bolts", seed);
    let server = serve(&mods, "shared-bolts-world");

    block_on(async {
        let mut first = joined(&server, "First").await;
        let mut second = joined(&server, "Second").await;
        await_bolts(&mut second, Some(&mut first), 2).await;
        await_bolts(&mut first, Some(&mut second), 2).await;
        let (mine, theirs) = (first.lightning_received(), second.lightning_received());
        assert_eq!(mine.len(), 2, "both bolts reach the first: {mine:?}");
        assert_eq!(theirs.len(), 2, "both bolts reach the second: {theirs:?}");

        assert_eq!(mine[0].seed, seed, "the seed crossed Lua by its bits");
        assert_eq!(mine[0].branches, 8);
        assert_ne!(mine[1].seed, 0, "the engine picked a seed");
        assert_ne!(mine[1].seed, mine[0].seed);
        for (index, (a, b)) in mine.iter().zip(&theirs).enumerate() {
            assert_eq!(a, b, "bolt {index} differs on the wire");
            let (one, other) = (
                tiamat_core::lightning::build_path(a),
                tiamat_core::lightning::build_path(b),
            );
            assert_eq!(
                tiamat_core::lightning::path_hash(&one),
                tiamat_core::lightning::path_hash(&other),
                "bolt {index} was drawn two ways"
            );
            println!(
                "bolt {index}: seed {:#018x}, {} segments, path hash {:#018x}",
                a.seed,
                one.segments.len(),
                tiamat_core::lightning::path_hash(&one)
            );
        }
    });

    assert!(server.stop());
}

#[test]
fn a_flash_addressed_to_one_player_reaches_nobody_else() {
    // Weather ask W28: thunder and a flash kept out of a cave. Two players
    // in range of every strike; each strike is addressed to one of them,
    // and the other's client receives nothing.
    let mods = write_aimed_storm("aimed");
    let server = ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: scratch("aimed-world"),
        identity_path: None,
        max_players: 4,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(mods.clone()),
        enabled_mods: bot::fixture::enabled_mods_for(&mods).expect("the mod's manifest"),
        seed: Some(3),
        rcon: None,
        materials: Vec::new(),
        world_options: Vec::new(),
    })
    .expect("start");

    block_on(async {
        let join = |name: &'static str| async {
            let mut bot = Bot::connect(
                server.local_addr(),
                Identity::generate().expect("identity"),
                server.cert_fingerprint(),
            )
            .await
            .expect("connect");
            bot.join(name).await.expect("join");
            bot
        };
        let mut first = join("First").await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < deadline && first.flashes_received().is_empty() {
            let _ = tokio::time::timeout(Duration::from_millis(100), first.recv()).await;
        }
        assert_eq!(
            first.flashes_received().len(),
            1,
            "the strike addressed to the first"
        );

        let mut second = join("Second").await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < deadline && second.flashes_received().is_empty() {
            let _ = tokio::time::timeout(Duration::from_millis(100), second.recv()).await;
            let _ = tokio::time::timeout(Duration::from_millis(20), first.recv()).await;
        }
        assert_eq!(
            second.flashes_received().len(),
            1,
            "the strike addressed to the second"
        );
        // And twenty server ticks more for anything wrongly sent to the
        // first: the strike went out in the second's join tick, so anything
        // addressed wrongly is long since on the wire. Real time, not
        // `recv` timeouts — those return at once while an inbox has
        // anything in it, and a player is sent a state every tick.
        first.sleep_ticks(20).await;
        for _ in 0..10 {
            let _ = tokio::time::timeout(Duration::from_millis(20), first.recv()).await;
        }
        assert_eq!(
            first.flashes_received().len(),
            1,
            "a flash addressed to the second player reached the first"
        );
    });

    assert!(server.stop());
}

#[test]
#[expect(
    clippy::float_cmp,
    reason = "the values asserted are set, not computed"
)]
fn a_mods_weather_reaches_the_players_sky_once_per_change_and_clears_and_lightning_is_seen_in_reach()
 {
    let server = ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: scratch("world"),
        identity_path: None,
        max_players: 1,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(write_storm("mods")),
        enabled_mods: None,
        seed: Some(4),
        rcon: None,
        materials: Vec::new(),
        world_options: Vec::new(),
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
            bot.join("Watcher").await.expect("join");

            // Until the clear arrives, or long enough that it is missing
            // rather than late.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            while tokio::time::Instant::now() < deadline
                && !bot.sky_modifiers_received().contains(&None)
            {
                let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
            }

            let received = bot.sky_modifiers_received();
            assert_eq!(
                received.len(),
                2,
                "a modifier set twenty times is one message, and its clearing one more: {received:?}"
            );
            let storm = received[0].expect("the storm");
            assert_eq!(storm.intensity, 0.55);
            assert_eq!(storm.sky, [0.55, 0.58, 0.62]);
            assert_eq!(storm.sky_mix, 0.7);
            assert_eq!(storm.fog_distance, 0.6);
            assert_eq!(storm.ease_ticks, 400);
            assert_eq!(received[1], None, "nil clears it");

            let rain = bot.precipitation_received();
            assert_eq!(
                rain.len(),
                2,
                "rain set twenty times is one message, and its end one more: {rain:?}"
            );
            let storm = rain[0].expect("the rain");
            assert_eq!(storm.rate, 900.0);
            assert_eq!(storm.above, 18.0);
            assert_eq!(storm.ease_ticks, 200);
            assert_eq!(storm.burst.velocity, [3.0, -22.0, 0.0]);
            assert_eq!(rain[1], None, "nil stops it");

            let flashes = bot.flashes_received();
            assert_eq!(
                flashes.len(),
                1,
                "the strike beside the player and no other: {flashes:?}"
            );
            assert_eq!(flashes[0].intensity, 1.5);
            assert_eq!(flashes[0].colour, [0.9, 0.92, 1.0]);
            assert_eq!((flashes[0].attack_ticks, flashes[0].decay_ticks), (1, 6));
            bot.disconnect().await;
        });
    assert!(server.stop());
}

/// A mod that keeps a rainbow over everybody online: set every tick (which
/// must cost nothing), changed once, and cleared for a player named in its
/// schedule — weather ask W30.
const STANDING_RAINBOW: &str = r#"
local online = {}
game.register_on_player_join(function(event)
    online[event.player] = 0
end)
game.register_on_player_leave(function(event)
    online[event.player] = nil
end)
game.register_on_tick(function()
    for player, ticks in pairs(online) do
        online[player] = ticks + 1
        if ticks < 10 then
            game.set_rainbow(player, { intensity = 0.5, ease_ticks = 200 })
        elseif ticks < 20 then
            game.set_rainbow(player, { intensity = 9, ease_ticks = 40 })
        else
            game.set_rainbow(player, nil)
        end
    end
end)
"#;

/// A mod whose rainbow never ends, for the player who comes back to it.
const LASTING_RAINBOW: &str = r#"
local online = {}
game.register_on_player_join(function(event) online[event.player] = true end)
game.register_on_player_leave(function(event) online[event.player] = nil end)
game.register_on_tick(function()
    for player in pairs(online) do
        game.set_rainbow(player, { intensity = 0.6 })
    end
end)
"#;

/// Receives on `bot` until it holds `count` rainbows or ten seconds pass.
async fn await_rainbows(bot: &mut Bot, count: usize) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline && bot.rainbows_received().len() < count {
        let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
    }
}

#[test]
fn a_rainbow_reaches_the_player_once_per_change_and_nil_clears_it() {
    // Weather ask W30: one message for a rainbow set on every one of ten
    // ticks, one for its change (a strength past one clamped), one for nil.
    let mods = write_mod("rainbow-mods", STANDING_RAINBOW);
    let server = serve(&mods, "rainbow-world");
    block_on(async {
        let mut bot = joined(&server, "Watcher").await;
        await_rainbows(&mut bot, 3).await;
        // Room for a fourth to arrive, were the standing value re-sent.
        let _ = tokio::time::timeout(Duration::from_millis(500), bot.recv()).await;
        let received = bot.rainbows_received();
        assert_eq!(
            received.len(),
            3,
            "set ten times, changed once, cleared once: {received:?}"
        );
        let first = received[0].expect("the rainbow");
        assert_eq!((first.intensity, first.ease_ticks), (0.5, 200));
        let second = received[1].expect("the change");
        assert_eq!((second.intensity, second.ease_ticks), (1.0, 40));
        assert_eq!(received[2], None, "nil clears it");
        bot.disconnect().await;
    });
    assert!(server.stop());
}

#[test]
#[expect(
    clippy::float_cmp,
    reason = "the values asserted are set, not computed"
)]
fn a_player_who_rejoins_is_told_the_standing_rainbow_again() {
    // Weather ask W30: the slot is forgotten when its player leaves, so the
    // value the mod keeps setting reaches them again on their return, as
    // precipitation's does.
    let mods = write_mod("rainbow-rejoin-mods", LASTING_RAINBOW);
    let server = serve(&mods, "rainbow-rejoin-world");
    let seed = [0x5A; 32];
    block_on(async {
        for visit in 0..2 {
            let mut bot = Bot::connect(
                server.local_addr(),
                Identity::from_seed(&seed),
                server.cert_fingerprint(),
            )
            .await
            .expect("connect");
            bot.join("Returner").await.expect("join");
            await_rainbows(&mut bot, 1).await;
            let _ = tokio::time::timeout(Duration::from_millis(300), bot.recv()).await;
            let received = bot.rainbows_received();
            assert_eq!(received.len(), 1, "visit {visit}: {received:?}");
            assert_eq!(received[0].expect("the rainbow").intensity, 0.6);
            bot.disconnect().await;
            // Long enough for the server to have seen them go.
            tokio::time::sleep(Duration::from_millis(700)).await;
        }
    });
    assert!(server.stop());
}

/// A mod that holds a black sky with `stars` over a player (set on every tick
/// for ten, which must cost nothing), changes it to a modifier that says
/// nothing of the stars, then to one that names too many, and clears it — engine
/// ask World 44.
const STARRY_UNDERSIDE: &str = r#"
local online = {}
game.register_on_player_join(function(event)
    online[event.player] = 0
end)
game.register_on_player_leave(function(event)
    online[event.player] = nil
end)
game.register_on_tick(function()
    for player, ticks in pairs(online) do
        online[player] = ticks + 1
        if ticks < 10 then
            game.set_sky_modifier(player, { intensity = 0, sky = { 0, 0, 0 }, stars = 1, ease_ticks = 40 })
        elseif ticks < 20 then
            game.set_sky_modifier(player, { intensity = 0, sky = { 0, 0, 0 }, ease_ticks = 40 })
        elseif ticks < 30 then
            game.set_sky_modifier(player, { stars = 7 })
        else
            game.set_sky_modifier(player, nil)
        end
    end
end)
"#;

#[test]
fn a_modifiers_stars_reach_the_player_once_per_change_and_clearing_it_leaves_the_keyframes() {
    let mods = write_mod("stars-mods", STARRY_UNDERSIDE);
    let server = serve(&mods, "stars-world");
    block_on(async {
        let mut bot = joined(&server, "Watcher").await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < deadline && bot.sky_modifiers_received().len() < 4 {
            let _ = tokio::time::timeout(Duration::from_millis(100), bot.recv()).await;
        }
        // Room for a fifth to arrive, were a standing value re-sent.
        let _ = tokio::time::timeout(Duration::from_millis(500), bot.recv()).await;
        let received = bot.sky_modifiers_received();
        assert_eq!(
            received.len(),
            4,
            "set ten times, changed twice, cleared once: {received:?}"
        );
        assert_eq!(
            received[0].expect("the underside").stars,
            Some(1.0),
            "the stars named reach the player"
        );
        assert_eq!(
            received[1].expect("without stars").stars,
            None,
            "a modifier with no say leaves the stars to the keyframes"
        );
        assert_eq!(
            received[2].expect("too many stars").stars,
            Some(1.0),
            "clamped at the server, so the client never sees past 1"
        );
        assert_eq!(
            received[3], None,
            "nil clears the modifier, and with it the stars"
        );
        bot.disconnect().await;
    });
    assert!(server.stop());
}
