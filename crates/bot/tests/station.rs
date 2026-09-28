// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Progress ask 4: a bot that can play a station, without a screen.
//!
//! The mod's brief is paced by a bot playing Craft's loop — but until now the
//! `bot` script API could join, move, dig, place, chat and press action keys,
//! and nothing more: it could not light a fire, open a kiln, or read what a
//! mod said back. Three calls close that: `bot.use(x, y, z)` (the place
//! control on a block with nothing to place, reaching `register_on_use`),
//! `bot.press(form, name)` (a button in a dialog a mod showed the bot), and
//! `bot.heard()` (the chat lines sent to the bot since the last call).
//!
//! The fixture below is not the reference mods — a small `station` mod, in
//! the style `mod_hooks.rs` and `dialogs.rs` use, that answers a use and a
//! dialog press by chatting back what it heard. A bot script drives it the
//! way a real scenario would: through `bot.*`, never through the library
//! directly, because the library path is what every OTHER test here already
//! proves.

use std::path::PathBuf;
use std::time::Duration;

use bot::{Bot, Channel, ScriptOutcome};
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::interest::ViewDistance;
use tiamat_server::{ServerHandle, Settings};

/// Where the station block sits — beside spawn and within reach, the same
/// spot `mod_hooks.rs`'s own use test picks for the same reason.
const STATION: tiamat_core::BlockPos = tiamat_core::BlockPos::new(2, -1, 0);

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamat-station").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// A mod that opens a dialog and drops a station block for every joining
/// player, then answers a use and a press by chatting back what it heard.
///
/// The join hook also chats `"dialog: shown"` once both are in place, so a
/// script has something to poll for with `bot.heard()` rather than guessing
/// how many ticks a join hook takes to run.
fn write_station(name: &str) -> PathBuf {
    let root = scratch(name);
    let dir = root.join("warden");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"warden\"\nname = \"Warden\"\nversion = \"0.1.0\"\n\
         license = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        r#"
local ground = game.register_block{ id = "ground" }
game.register_block{ id = "station" }
game.register_on_generate(function(buf, pos)
    buf:fill_below_heightmap(game.flat_heightmap(0), ground)
end)
-- An empty world is not a neutral fixture: with nothing to stand on the
-- player free-falls and the station goes out of reach before a script gets
-- to it. `mod_hooks.rs` learned this first.
game.register_tool{
    id = "hand",
    brush = "block",
    speed_multiplier = 1.0,
    default = true,
}

game.register_on_player_join(function(event)
    game.set_block({ x = 2, y = -1, z = 0 }, "warden:station")
    game.show_dialog{
        player = event.player,
        form = "panel",
        tree = {
            type = "container", direction = "column",
            children = {
                { type = "button", name = "go", text = "Go" },
            },
        },
    }
    game.chat_to(event.player, "dialog: shown")
end)

game.register_on_use(function(e)
    game.chat_to(e.player, "used: station")
end)

game.register_on_dialog_event(function(event)
    if event.kind == "pressed" and event.name == "go" then
        game.chat_to(event.player, "pressed: go")
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
        max_players: 4,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(write_station(name)),
        enabled_mods: None,
        seed: Some(13),
        rcon: None,
        materials: Vec::new(),
        world_options: Vec::new(),
    })
    .expect("start")
}

/// A multi-thread runtime, unlike the current-thread `block_on` most tests in
/// this crate use. `bot::run_script` blocks the calling thread on the script
/// channel while [`bot::drive`] answers it from a spawned task — on a
/// current-thread runtime the two would starve each other.
fn multi_thread_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime")
}

/// Connects a fresh bot, runs `source` against it under `name`, and waits for
/// the driver to finish. Mirrors `bot run`'s own wiring in `src/main.rs`.
fn run_bot_script(
    runtime: &tokio::runtime::Runtime,
    server: &ServerHandle,
    name: &str,
    source: &str,
) -> ScriptOutcome {
    let identity = Identity::generate().expect("identity");
    let client = runtime
        .block_on(Bot::connect(
            server.local_addr(),
            identity,
            server.cert_fingerprint(),
        ))
        .expect("connect");

    let (channel, commands, replies) = Channel::pair();
    let driver = runtime.spawn(bot::drive(client, commands, replies));

    let full_source = format!("bot.join('{name}')\n{source}");
    let outcome = bot::run_script(&full_source, name, channel).expect("the VM should start");

    // Dropping `channel` at the end of `run_script` ends the driver; give it
    // a bounded moment to notice rather than hanging the test if it does not.
    runtime.block_on(async {
        let _ = tokio::time::timeout(Duration::from_secs(5), driver).await;
    });

    outcome
}

/// Polls `bot.heard()` up to a hundred ticks (five seconds) for a line equal
/// to `want`, breaking as soon as it is seen. Written into the script rather
/// than the harness, because waiting for a chat line is exactly the thing
/// `bot.heard()` is for.
fn wait_for_lua(var: &str, want: &str) -> String {
    format!(
        "local {var} = false\n\
         for _i = 1, 100 do\n\
         \x20   for _, line in ipairs(bot.heard()) do\n\
         \x20       if line == '{want}' then {var} = true end\n\
         \x20   end\n\
         \x20   if {var} then break end\n\
         \x20   bot.sleep_ticks(1)\n\
         end\n"
    )
}

#[test]
fn a_use_reaches_on_use_and_heard_returns_the_reply_then_drains() {
    let server = start("use-and-heard");
    let runtime = multi_thread_runtime();

    let source = format!(
        "{wait_shown}\
         bot.assert(shown, 'the join dialog never announced itself')\n\
         bot.use({x}, {y}, {z})\n\
         {wait_used}\
         bot.assert(used, 'on_use never chatted back')\n\
         bot.assert(#bot.heard() == 0, 'heard() must drain rather than repeat a line')\n\
         bot.disconnect()\n",
        wait_shown = wait_for_lua("shown", "dialog: shown"),
        wait_used = wait_for_lua("used", "used: station"),
        x = STATION.x,
        y = STATION.y,
        z = STATION.z,
    );

    let outcome = run_bot_script(&runtime, &server, "Operator", &source);
    assert!(outcome.passed, "{:?}", outcome.failure);
    assert!(
        outcome.assertions >= 3,
        "expected at least 3 assertions, counted {}",
        outcome.assertions
    );

    server.stop();
}

#[test]
fn a_press_reaches_the_dialog_it_was_shown_and_the_mod_chats_back() {
    let server = start("press-and-heard");
    let runtime = multi_thread_runtime();

    let source = format!(
        "{wait_shown}\
         bot.assert(shown, 'the join dialog never announced itself')\n\
         bot.press('warden:panel', 'go')\n\
         {wait_pressed}\
         bot.assert(pressed, 'the press never reached the mod')\n\
         bot.disconnect()\n",
        wait_shown = wait_for_lua("shown", "dialog: shown"),
        wait_pressed = wait_for_lua("pressed", "pressed: go"),
    );

    let outcome = run_bot_script(&runtime, &server, "Presser", &source);
    assert!(outcome.passed, "{:?}", outcome.failure);

    server.stop();
}

#[test]
fn pressing_a_form_the_bot_was_never_shown_errors() {
    // The forgery-shaped half, from the honest side: a script that names a
    // form it holds no dialog for learns that at the call — a Lua error that
    // stops the script — rather than sending an event the server silently
    // drops for owning no such form.
    let server = start("press-unknown-form");
    let runtime = multi_thread_runtime();

    let outcome = run_bot_script(
        &runtime,
        &server,
        "Forger",
        "bot.press('warden:nosuchform', 'go')\nbot.disconnect()",
    );

    assert!(
        !outcome.passed,
        "pressing a form the bot never held must fail the script"
    );
    let failure = outcome.failure.expect("a failure message");
    assert!(
        failure.contains("warden:nosuchform"),
        "the failure should name the form that was never shown: {failure}"
    );

    server.stop();
}
