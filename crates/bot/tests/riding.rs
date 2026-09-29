// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Riding, over a real server — Life ask 18.
//!
//! A test mod stands a scarecrow beside each player and seats whoever uses it.
//! From then on the player's keys drive the SCARECROW — at its own half pace,
//! through the step every entity takes — and the position the server reports
//! for the player is the seat on top of it. Sneak gets them off, and so do
//! `game.dismount` and the scarecrow being despawned under them, each heard by
//! `on_dismount` with its reason and where the rider was put. The rules for a
//! mount that cannot be had are answered as `nil, reason`, and a player who
//! leaves while riding frees the mount.

use std::path::{Path, PathBuf};
use std::time::Duration;

use bot::Bot;
use bot::client::PlayerPosition;
use tiamat_core::BlockPos;
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::interest::ViewDistance;
use tiamat_core::proto::actions;
use tiamat_server::{ServerHandle, Settings};

const PATIENCE: Duration = Duration::from_secs(30);

/// The seat the mod asks for, in blocks above the scarecrow's feet.
const SEAT: f64 = 2.0;

/// The scarecrow's pace, as a multiple of a player's.
const PACE: f64 = 0.5;

/// Marked by the mod when a ride ends because its rider left.
const LEFT: BlockPos = BlockPos::new(1, 10, 1);

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamat-riding").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Flat ground, a scarecrow two blocks north of each player once they stand,
/// and the verbs a test drives a ride with.
fn write_stable(root: &Path) -> PathBuf {
    let mods = root.join("mods");
    let dir = mods.join("stable");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"stable\"\nname = \"Stable\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        format!(
            r#"
local ground = game.register_block{{ id = "ground" }}
game.register_block{{ id = "mark" }}
game.register_on_generate(function(buf, pos)
    buf:fill_below_heightmap(game.flat_heightmap(0), ground)
end)
game.register_tool{{ id = "hand", brush = "block", speed_multiplier = 1.0, default = true }}

local SEAT = {{ y = {SEAT} }}

-- One scarecrow per player, put up the first tick their body stands.
local placed = {{}}
game.register_on_player_join(function(event)
    placed[event.player] = false
end)
game.register_on_tick(function()
    for uuid, done in pairs(placed) do
        if not done then
            local id = game.player_entity(uuid)
            local body = id and game.entity(id)
            if body and body.on_ground then
                local scarecrow = game.spawn_entity{{
                    pos = {{ x = body.pos.x, y = body.pos.y, z = body.pos.z + 2 }},
                    model = "engine:humanoid",
                    collider = {{ width = 2.4, height = 6 }},
                    speed = {PACE},
                }}
                if scarecrow ~= nil then
                    placed[uuid] = true
                    game.chat_to(uuid, "scarecrow " .. scarecrow)
                end
            end
        end
    end
end)

local function said(player, ok, why)
    game.chat_to(player, "mount " .. tostring(ok) .. " " .. tostring(why))
end

-- Using the scarecrow is getting on it.
game.register_on_use_entity(function(event)
    said(event.player, game.mount(event.player, event.target, {{ seat = SEAT }}))
    return ""
end)

game.register_on_dismount(function(event)
    game.chat_to(event.player, string.format(
        "dismount %s %d %.4f %.4f %.4f", event.reason, event.entity, event.x, event.y, event.z))
    if event.reason == "leave" then
        game.set_block({{ x = 1, y = 10, z = 1 }}, "stable:mark")
    end
end)

game.register_on_chat(function(event)
    local text = event.text
    local id = tonumber(text:match("^mount (%d+)$"))
    if id then
        said(event.player, game.mount(event.player, id, {{ seat = SEAT }}))
    elseif text == "self" then
        said(event.player, game.mount(event.player, game.player_entity(event.player)))
    elseif text == "off" then
        game.chat_to(event.player, "off " .. tostring(game.dismount(event.player)))
    elseif text == "gone" then
        game.despawn_entity(game.mounted(event.player))
    elseif text:match("^where (%d+)$") then
        local found = game.entity(tonumber(text:match("^where (%d+)$")))
        if found then
            game.chat_to(event.player, string.format(
                "at %.4f %.4f %.4f", found.pos.x, found.pos.y, found.pos.z))
        else
            game.chat_to(event.player, "at nil")
        end
    elseif text == "which" then
        game.chat_to(event.player, "riding " .. tostring(game.mounted(event.player)))
    else
        return
    end
    return false
end)
"#
        ),
    )
    .expect("script");
    mods
}

fn start(name: &str) -> ServerHandle {
    let root = scratch(name);
    let mods = write_stable(&root);
    ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: root.join("world"),
        identity_path: None,
        max_players: 4,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(mods),
        enabled_mods: None,
        seed: Some(23),
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

async fn join(server: &ServerHandle, name: &str) -> Bot {
    join_as(server, name, Identity::generate().expect("identity")).await
}

async fn join_as(server: &ServerHandle, name: &str, identity: Identity) -> Bot {
    let mut bot = Bot::connect(server.local_addr(), identity, server.cert_fingerprint())
        .await
        .expect("connect");
    bot.join(name).await.expect("join");
    bot
}

/// Reads until a notice starting with `prefix` arrives that was not there
/// before `seen` of them had been, and returns the rest of it.
async fn notice_after(bot: &mut Bot, seen: usize, prefix: &str) -> String {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        if let Some(found) = bot
            .notices()
            .into_iter()
            .skip(seen)
            .find_map(|text| text.strip_prefix(prefix).map(str::to_owned))
        {
            return found;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no `{prefix}` notice arrived; notices {:?}",
            bot.notices()
        );
        let _ = tokio::time::timeout(Duration::from_millis(200), bot.recv()).await;
    }
}

/// Says something to the mod and waits for the answer that starts `prefix`.
async fn ask(bot: &mut Bot, text: &str, prefix: &str) -> String {
    let seen = bot.notices().len();
    bot.chat(text).await.expect("send");
    notice_after(bot, seen, prefix).await
}

/// Waits for this player's scarecrow and says which entity it is.
async fn scarecrow(bot: &mut Bot) -> u64 {
    let id = notice_after(bot, 0, "scarecrow ").await;
    let id = id.parse().expect("an entity id");
    bot.expect_entity(|entity| entity.id == id, PATIENCE)
        .await
        .expect("the scarecrow should be sent to the player beside it");
    id
}

/// A position in world blocks.
fn blocks(position: &PlayerPosition) -> [f64; 3] {
    let corner = BlockPos::from_chunk_corner(position.chunk);
    let cells = f64::from(tiamat_core::SUBNODES_PER_AXIS);
    [
        f64::from(corner.x) + f64::from(position.local[0]) / cells,
        f64::from(corner.y) + f64::from(position.local[1]) / cells,
        f64::from(corner.z) + f64::from(position.local[2]) / cells,
    ]
}

/// Where the server has an entity, in world blocks, asked of the mod.
///
/// **Asked rather than read off the entity stream**, which is a view for
/// drawing: it is sent only when a body moves a twelfth of a cell in one
/// tick, so the last creep of a body coasting to a stop is never sent, and a
/// test comparing a rider with the stream's copy of their mount measures that
/// rather than the ride.
async fn entity_at(bot: &mut Bot, id: u64) -> Option<[f64; 3]> {
    let answer = ask(bot, &format!("where {id}"), "at ").await;
    if answer == "nil" {
        return None;
    }
    let axes: Vec<f64> = answer
        .split_whitespace()
        .map(|axis| axis.parse().expect("a coordinate"))
        .collect();
    Some([axes[0], axes[1], axes[2]])
}

/// Stands still until both the rider and the scarecrow have stopped, and
/// returns where the server has the player.
async fn settle(bot: &mut Bot) -> [f64; 3] {
    /// Ticks at one place that count as standing still.
    const REST: usize = 4;
    /// How long to wait for it: a walk leaves keys queued ahead of the
    /// server, and a body coasts after them.
    const PATIENCE: usize = 120;
    // **All three axes, not only the height** — `Bot::settle` waits out a
    // jump, and a ride is a walk: it is still going when its height is not.
    let mut here = bot.walk([0.0; 3], 0, 1).await.expect("stand");
    let mut still = 1;
    for _ in 0..PATIENCE {
        if still >= REST {
            break;
        }
        let now = bot.walk([0.0; 3], 0, 1).await.expect("stand");
        if now == here {
            still += 1;
        } else {
            still = 1;
            here = now;
        }
    }
    // A few more ticks for the scarecrow's own state to reach the bot, which
    // travels on the entity stream rather than with the player's state.
    blocks(&bot.walk([0.0; 3], 0, 6).await.expect("stand"))
}

fn assert_near(what: &str, got: [f64; 3], want: [f64; 3], within: f64) {
    let off = [got[0] - want[0], got[1] - want[1], got[2] - want[2]];
    assert!(
        off.iter().all(|axis| axis.abs() <= within),
        "{what}: at {got:?}, expected {want:?} (off by {off:?})"
    );
}

/// The parsed `dismount` notice: reason, entity, and where.
fn dismount(line: &str) -> (String, u64, [f64; 3]) {
    let words: Vec<&str> = line.split_whitespace().collect();
    assert_eq!(words.len(), 5, "a dismount notice reads `{line}`");
    let at = |index: usize| words[index].parse::<f64>().expect("a coordinate");
    (
        words[0].to_owned(),
        words[1].parse().expect("an entity id"),
        [at(2), at(3), at(4)],
    )
}

#[test]
fn a_rider_drives_the_mount_at_its_pace_and_sits_on_it_until_they_get_off() {
    let server = start("drive");
    block_on(async {
        let mut bot = join(&server, "Rider").await;
        let mount = scarecrow(&mut bot).await;
        let home = settle(&mut bot).await;

        // On foot first, for the pace to be compared against: east and back,
        // so the scarecrow is in front of the bot again afterwards.
        let east = [1.0, 0.0, 0.0];
        let west = [-1.0, 0.0, 0.0];
        // Measured at rest both times: a walk leaves a few ticks of its keys
        // queued ahead of the server, and a body coasts before it stops, and
        // both are part of the same curve a pace scales.
        bot.walk(east, 0, 30).await.expect("walk");
        let walked = settle(&mut bot).await;
        let on_foot = walked[0] - home[0];
        bot.walk(west, 0, 30).await.expect("walk back");
        let _ = settle(&mut bot).await;
        assert!(on_foot > 2.0, "the bot never walked: {on_foot} blocks");

        // Using it is getting on it.
        let seen = bot.notices().len();
        bot.use_at_nothing().await.expect("send");
        let answer = notice_after(&mut bot, seen, "mount ").await;
        assert_eq!(answer, "true nil", "the mount was refused");
        assert_eq!(ask(&mut bot, "which", "riding ").await, mount.to_string());
        let sat = settle(&mut bot).await;
        let under = entity_at(&mut bot, mount).await.expect("the scarecrow");
        assert_near(
            "a rider sits at the seat",
            sat,
            [under[0], under[1] + SEAT, under[2]],
            0.05,
        );
        // **And the player's state says so** (protocol v80): the mount's own
        // body, which is the one a client predicts, under the seat it reports.
        let ride = bot.riding().expect("a rider's state carries the mount");
        assert_eq!(ride.entity, mount);
        let feet = blocks(&PlayerPosition {
            chunk: ride.chunk,
            local: ride.local,
        });
        assert_near("the state's mount is the scarecrow", feet, under, 0.001);
        assert_near(
            "the state's rider is on the state's mount",
            sat,
            [feet[0], feet[1] + SEAT, feet[2]],
            0.001,
        );
        assert_eq!(ride.size, [2.4, 6.0], "the mount's own box");
        assert!((f64::from(ride.speed) - PACE).abs() < 1e-6, "its own pace");

        // The keys drive the scarecrow now, at its own pace.
        let before = under;
        let rode = blocks(&bot.walk(east, 0, 30).await.expect("ride"));
        let there = settle(&mut bot).await;
        let moved = entity_at(&mut bot, mount).await.expect("the scarecrow");
        let ridden = moved[0] - before[0];
        assert!(
            ridden > 0.5,
            "the scarecrow did not go where the rider's keys said: {ridden} blocks \
             (the rider was reported at {rode:?})"
        );
        let pace = ridden / on_foot;
        assert!(
            (pace - PACE).abs() < 0.15,
            "the ride went {ridden} blocks against {on_foot} on foot: a pace of {pace}, not {PACE}"
        );
        assert_near(
            "the rider rode with it",
            there,
            [moved[0], moved[1] + SEAT, moved[2]],
            0.05,
        );

        // Sneak gets them off, at the scarecrow's feet, and says so.
        let seen = bot.notices().len();
        bot.walk([0.0; 3], actions::SNEAK, 2).await.expect("sneak");
        let (reason, entity, at) = dismount(&notice_after(&mut bot, seen, "dismount ").await);
        assert_eq!((reason.as_str(), entity), ("sneak", mount));
        let stood = entity_at(&mut bot, mount).await.expect("the scarecrow");
        assert_near("the sneak landed them at its feet", at, stood, 0.05);
        let off = settle(&mut bot).await;
        assert!(
            (off[1] - stood[1]).abs() < 0.2,
            "off the scarecrow the rider stands at {off:?}, the scarecrow at {stood:?}"
        );
        assert_eq!(ask(&mut bot, "which", "riding ").await, "nil");
        assert_eq!(bot.riding(), None, "a state on foot still carried a mount");
        // And the keys are the player's own again: the player walks away and
        // the scarecrow does not come too. It may lean the other way — they
        // were standing inside it, and the crowd pass eases the two apart.
        bot.walk(west, 0, 10).await.expect("walk");
        let away = settle(&mut bot).await;
        let left = entity_at(&mut bot, mount).await.expect("the scarecrow");
        assert!(
            away[0] < stood[0] - 1.0,
            "the rider did not walk off: at {away:?}, from {stood:?}"
        );
        assert!(
            left[0] > stood[0] - 0.05,
            "the scarecrow followed its old rider's keys to {left:?} from {stood:?}"
        );

        // `game.dismount`, the same way.
        assert_eq!(
            ask(&mut bot, &format!("mount {mount}"), "mount ").await,
            "true nil"
        );
        let _ = settle(&mut bot).await;
        let seen = bot.notices().len();
        assert_eq!(ask(&mut bot, "off", "off ").await, "true");
        let (reason, _, at) = dismount(&notice_after(&mut bot, seen, "dismount ").await);
        assert_eq!(reason, "dismount");
        assert_near(
            "a dismount lands them at its feet",
            at,
            entity_at(&mut bot, mount).await.expect("the scarecrow"),
            0.05,
        );
        assert_eq!(ask(&mut bot, "off", "off ").await, "false", "off twice");

        // And the scarecrow despawned under them drops them where it stood.
        assert_eq!(
            ask(&mut bot, &format!("mount {mount}"), "mount ").await,
            "true nil"
        );
        let _ = settle(&mut bot).await;
        let last = entity_at(&mut bot, mount).await.expect("the scarecrow");
        let seen = bot.notices().len();
        bot.chat("gone").await.expect("send");
        let (reason, entity, at) = dismount(&notice_after(&mut bot, seen, "dismount ").await);
        assert_eq!((reason.as_str(), entity), ("gone", mount));
        assert_near(
            "a vanished mount drops its rider where it stood",
            at,
            last,
            0.05,
        );
        let dropped = settle(&mut bot).await;
        assert_near("and the rider stays there", dropped, last, 0.2);
        assert_eq!(ask(&mut bot, "which", "riding ").await, "nil");
        assert!(
            entity_at(&mut bot, mount).await.is_none(),
            "the despawned scarecrow is still there"
        );
        assert!(
            !bot.entities().contains_key(&mount),
            "the despawned scarecrow is still being drawn"
        );
    });
    assert!(server.stop());
}

#[test]
fn a_mount_that_cannot_be_had_is_refused_with_a_reason_and_leaving_frees_it() {
    let server = start("rules");
    block_on(async {
        let who = Identity::generate().expect("identity").seed();
        let mut first = join_as(&server, "First", Identity::from_seed(&who)).await;
        let theirs = scarecrow(&mut first).await;
        let mut second = join(&server, "Second").await;
        let spare = scarecrow(&mut second).await;
        let mark = second
            .material_table()
            .expect("a material table on join")
            .into_iter()
            .find(|entry| entry.name == "stable:mark")
            .map(|entry| entry.id)
            .expect("the mod registers a mark");

        assert_eq!(
            ask(&mut first, &format!("mount {theirs}"), "mount ").await,
            "true nil"
        );
        // One rider to a mount.
        assert_eq!(
            ask(&mut second, &format!("mount {theirs}"), "mount ").await,
            "nil ridden"
        );
        // One mount to a rider.
        assert_eq!(
            ask(&mut first, &format!("mount {spare}"), "mount ").await,
            "nil already riding"
        );
        // Nothing there, and somebody's own body.
        assert_eq!(
            ask(&mut first, "mount 123456789", "mount ").await,
            "nil no such entity"
        );
        assert_eq!(ask(&mut second, "self", "mount ").await, "nil a player");
        // Asking again for the one already ridden keeps the ride.
        assert_eq!(
            ask(&mut first, &format!("mount {theirs}"), "mount ").await,
            "true nil"
        );
        assert_eq!(
            ask(&mut first, "which", "riding ").await,
            theirs.to_string()
        );

        // **Leaving dismounts.** Nothing about a ride is saved: the mod hears
        // it end, and the scarecrow is anybody's.
        first.disconnect().await;
        second
            .expect_block(LEFT, mark, PATIENCE)
            .await
            .expect("the mod should hear the ride end when its rider left");
        assert_eq!(
            ask(&mut second, &format!("mount {theirs}"), "mount ").await,
            "true nil",
            "a mount whose rider left is still taken"
        );

        // And the one who left comes back on foot.
        let mut back = join_as(&server, "First", Identity::from_seed(&who)).await;
        let _ = back.settle().await;
        assert_eq!(ask(&mut back, "which", "riding ").await, "nil");
    });
    assert!(server.stop());
}

/// A multi-thread runtime: `bot::run_script` blocks the calling thread on the
/// script channel while `bot::drive` answers it from a spawned task, and on a
/// current-thread runtime the two would starve each other (see `station.rs`).
fn multi_thread_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime")
}

#[test]
fn a_scripted_bot_gets_on_ahead_of_it_asks_what_it_rides_and_sneaks_off() {
    // The `bot.*` verbs a pacing script rides with, against a real server:
    // `use_ahead` is the right-click at what the bot faces — its scarecrow,
    // two blocks north, where the mod seats whoever uses it — `mounted` is
    // the server's latest word, and `sneak` gets it off.
    let server = start("script");
    let runtime = multi_thread_runtime();
    let client = runtime
        .block_on(Bot::connect(
            server.local_addr(),
            Identity::generate().expect("identity"),
            server.cert_fingerprint(),
        ))
        .expect("connect");
    let (channel, commands, replies) = bot::Channel::pair();
    let driver = runtime.spawn(bot::drive(client, commands, replies));
    let source = r#"
bot.join('Scripted')
local id
for _ = 1, 400 do
    for _, line in ipairs(bot.heard()) do
        local found = line:match("^scarecrow (%d+)$")
        if found then id = tonumber(found) end
    end
    if id then break end
    bot.sleep_ticks(1)
end
bot.assert(id ~= nil, "the mod never put up a scarecrow")
bot.assert(bot.mounted() == nil, "riding before getting on")
bot.sleep_ticks(10)
bot.use_ahead()
local riding
for _ = 1, 100 do
    riding = bot.mounted()
    if riding then break end
    bot.sleep_ticks(1)
end
bot.assert(riding == id, "riding " .. tostring(riding) .. ", not the scarecrow " .. tostring(id))
bot.sneak()
local off = false
local said = {}
for _ = 1, 100 do
    for _, line in ipairs(bot.heard()) do said[#said + 1] = line end
    if bot.mounted() == nil then off = true break end
    bot.sleep_ticks(1)
end
bot.assert(off, "sneaking did not get the bot off; the mod said: " .. table.concat(said, " | "))
"#;
    let outcome = bot::run_script(source, "ride", channel).expect("the VM should start");
    runtime.block_on(async {
        let _ = tokio::time::timeout(Duration::from_secs(5), driver).await;
    });
    assert!(outcome.passed, "{:?}", outcome.failure);
    assert_eq!(outcome.assertions, 4);
    assert!(server.stop());
}
