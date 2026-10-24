// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Life ask 23, part 3: a player lies down to go under a one-block gap when a
//! mod grants it, over a real server.
//!
//! What is measured is the SERVER's answer — where it puts the body, and what
//! `game.entity(body).crawling` and the animation tag say — because that is
//! the authority. The client predicting the same posture is the other half
//! and is tested where the predictor is (`client/src/predict.rs`).

use std::path::PathBuf;

use bot::Bot;
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::interest::ViewDistance;
use tiamat_core::proto::actions;
use tiamat_server::{ServerHandle, Settings};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamat-crawl").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// The spawn chunk's floor is the top of the ground at world y = −1, so the
/// body stands with its feet at cell 0. The slab is one block up, cells 3 to
/// 5, over blocks z = 6..=9: a passage one block high and twelve cells long,
/// its mouth at cell 18, north of the spawn at cell 1.5.
const MOUTH: f32 = 18.0;
const FAR_END: f32 = 30.0;

/// Flat ground with the slab, and a mod that grants the crawl on a chat word
/// and reports the body's posture on another.
fn write_mod(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("crawl");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"crawl\"\nname = \"Crawl\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        r#"
local ground = game.register_block{ id = "ground" }
game.register_on_generate(function(buf, pos)
    buf:fill_below_heightmap(game.flat_heightmap(0), ground)
    if pos.x == 0 and pos.y == 0 and pos.z == 0 then
        for x = 0, 15 do
            for z = 6, 9 do
                buf:set_block(x, 1, z, ground)
            end
        end
    end
end)

game.register_on_chat(function(event)
    if event.text == "crawl" then
        game.set_player_abilities(event.player, { crawl = true })
        return ""
    elseif event.text == "posture" then
        local body = game.entity(game.player_entity(event.player))
        game.chat_to(event.player, string.format("posture %s %d", tostring(body.crawling), body.anim))
        return ""
    end
end)
"#,
    )
    .expect("script");
    root
}

fn start(name: &str) -> ServerHandle {
    ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: scratch(&format!("{name}-world")),
        identity_path: None,
        max_players: 2,
        allow_custom_skins: false,
        sticky_names: false,
        name_policy: tiamat_core::session::NamePolicy::Off,
        registry_trust: None,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(write_mod(&format!("{name}-mods"))),
        enabled_mods: None,
        seed: Some(5),
        rcon: None,
        materials: Vec::new(),
        world_options: Vec::new(),
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

async fn connect(server: &ServerHandle, name: &str) -> Bot {
    let mut bot = Bot::connect(
        server.local_addr(),
        Identity::generate().expect("identity"),
        server.cert_fingerprint(),
    )
    .await
    .expect("connect");
    bot.join(name).await.expect("join");
    bot.sleep_ticks(10).await;
    bot
}

fn notice_after(bot: &Bot, seen: usize, prefix: &str) -> Option<String> {
    bot.notices()
        .into_iter()
        .skip(seen)
        .rev()
        .find_map(|text| text.strip_prefix(prefix).map(str::to_owned))
}

/// The posture and the animation tag as the mod reads them off the body.
async fn posture(bot: &mut Bot) -> (bool, u8) {
    let seen = bot.notices().len();
    bot.chat("posture").await.expect("chat");
    for _ in 0..40 {
        if let Some(answer) = notice_after(bot, seen, "posture ") {
            let words: Vec<&str> = answer.split_whitespace().collect();
            return (words[0] == "true", words[1].parse().expect("a tag"));
        }
        bot.walk([0.0; 3], 0, 1).await.expect("wait");
    }
    panic!("the mod never answered `posture`");
}

/// Sneaks north for `ticks` and answers the body's world z, in cells.
async fn sneak_north(bot: &mut Bot, ticks: u64) -> f32 {
    let at = bot
        .walk([0.0, 0.0, 1.0], actions::SNEAK, ticks)
        .await
        .expect("sneak");
    at.chunk.z as f32 * tiamat_core::CHUNK_SUBNODES as f32 + at.local[2]
}

/// Sneaks north, twenty ticks at a time, until the body's z passes `past` or
/// `rounds` of twenty have gone by. A sneak covers 0.195 cells a tick, so
/// twenty ticks is about four cells; the budget is what a loaded runner
/// needs, not what the walk takes.
async fn sneak_north_past(bot: &mut Bot, past: f32, rounds: usize) -> f32 {
    let mut z = sneak_north(bot, 1).await;
    for _ in 0..rounds {
        if z > past {
            break;
        }
        z = sneak_north(bot, 20).await;
    }
    z
}

#[test]
fn a_one_block_gap_stops_a_body_the_mod_has_not_let_crawl() {
    let server = start("ungranted");
    block_on(async {
        let mut bot = connect(&server, "Walker").await;
        // Enough sneaking to be well past the mouth, were the slab not there;
        // the body is stopped at it instead.
        let z = sneak_north_past(&mut bot, MOUTH, 12).await;
        assert!(
            z < MOUTH && z > MOUTH - 4.0,
            "an ungranted body was not stopped at the mouth: z = {z}"
        );
        let (crawling, tag) = posture(&mut bot).await;
        assert!(!crawling, "nothing crawls without the grant");
        assert_ne!(tag, tiamat_core::ent::AnimTag::CRAWL.0);
        bot.disconnect().await;
    });
    assert!(server.stop());
}

#[test]
fn a_granted_body_lies_down_through_the_gap_and_stands_beyond_it() {
    let server = start("granted");
    block_on(async {
        let mut bot = connect(&server, "Crawler").await;
        bot.chat("crawl").await.expect("chat");
        bot.sleep_ticks(10).await;

        // Under the slab: a few cells past the mouth, well short of the far end.
        let z = sneak_north_past(&mut bot, MOUTH + 3.0, 12).await;
        assert!(
            z > MOUTH && z < FAR_END,
            "a granted body did not get under the slab: z = {z}"
        );
        let (crawling, tag) = posture(&mut bot).await;
        assert!(crawling, "the body under the slab is not lying down");
        assert_eq!(
            tag,
            tiamat_core::ent::AnimTag::CRAWL.0,
            "a crawling body is drawn crawling"
        );

        // Out the far side, and standing again.
        let z = sneak_north_past(&mut bot, FAR_END + 1.0, 12).await;
        assert!(z > FAR_END + 1.0, "the body never came out: z = {z}");
        let (crawling, tag) = posture(&mut bot).await;
        assert!(!crawling, "a body with room over it stands");
        assert_ne!(tag, tiamat_core::ent::AnimTag::CRAWL.0);
        bot.disconnect().await;
    });
    assert!(server.stop());
}
