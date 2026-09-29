// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Driving a bot from a Lua script.
//!
//! A bot session is a real protocol client plus a script telling it what to do.
//! The `bot.*` table below is the whole surface; anything a scenario needs that
//! is not here is a gap in the harness, not a reason to reach around it.
//!
//! # The sandbox, and what it is actually for
//!
//! Bot scripts run with the same globals removed that server mods lose: `io`,
//! `os`, `package`, `dofile`, `loadfile`, `debug`. Be precise about why,
//! because the reason is **not** the same as it is for mods.
//!
//! A server mod is code an operator installed from somewhere; a client script
//! pushed by a server is outright hostile input (charter rule 14). A bot script
//! is a test file you wrote and chose to run — the sandbox is not a security
//! boundary there, it is defence in depth and, more usefully, a guarantee that
//! a scenario script is *portable*: one that reaches for the filesystem works
//! on the machine that wrote it and nowhere else.
//!
//! The docs on this module previously claimed it "reuses the core sandboxed
//! runtime". It did not — it built a bare `Lua`, and `io.open` worked. Caught
//! by `a_script_cannot_reach_the_filesystem` below, which is why that test
//! enumerates globals rather than trusting the sentence above it.
//!
//! # Blocking calls, not callbacks
//!
//! `bot.dig_block(...)` returns when the server has confirmed the edit. Scripts
//! read top to bottom, which is what makes a failing assertion point at a line
//! rather than at a continuation. The cost is that a script cannot do two
//! things at once — which is what `swarm` is for.

use std::sync::mpsc;

use tiamat_core::proto::Press;
use tiamat_core::{BlockPos, SubNodePos};

/// One thing a script asked the bot to do.
///
/// The script thread sends these to the async side and waits for the reply.
/// Deliberately data rather than closures: a command that can be printed can be
/// recorded, and a command that can be recorded can be replayed.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Complete the join flow under this display name.
    Join(String),
    /// Dig a whole block.
    DigBlock(BlockPos),
    /// Dig one sub-node.
    DigSubNode(SubNodePos),
    /// Place a material as a whole block.
    Place(BlockPos, u16),
    /// Place one unit into one sub-node cell — the cell named, not the block's.
    ///
    /// The mirror of [`Command::DigSubNode`], and the other half of what makes
    /// carving reversible.
    PlaceSubNode(SubNodePos, u16),
    /// Wait for a block to hold a material.
    ExpectBlock(BlockPos, u16, u64),
    /// Wait for a block to be PARTIALLY filled, with a given cell count.
    ///
    /// The shape, not merely the presence: a scenario that only checked
    /// something appeared could not tell 13 spare nodes from a whole block,
    /// which is the entire distinction sub-nodes exist to make.
    ExpectPartial(BlockPos, u16, u32, u64),
    /// Report movement intent.
    MoveTo(f32, f32, f32),
    /// Send a chat line.
    Chat(String),
    /// Press or release a mod-registered action, by id.
    ///
    /// **The convergence Task 13 asks for.** A bot presses the same named
    /// action a player presses, so a scenario exercises the path a human does
    /// rather than a parallel one that can rot without anybody noticing. It
    /// takes the ID and not a key for exactly the reason charter rule 11 gives
    /// mods names instead of keys: a script written against a key would break
    /// the moment somebody rebound it.
    Action(String, bool),
    /// Wait for the server to advance.
    SleepTicks(u32),
    /// Ask for the current inventory.
    Inventory,
    /// Ask what the client holds around itself: full chunks, summaries,
    /// unloads, within this many chunks of the player.
    ChunkReport(i32),
    /// Wait until the inventory holds at least this many units of a material.
    ExpectUnits(u16, u32, u64),
    /// The place control on a block with nothing to place — a real client's
    /// right-click with an empty hand, which reaches a mod's
    /// `register_on_use`.
    ///
    /// **Progress ask 4.** Aimed at one of the block's 27 sub-node cells,
    /// `(dx, dy, dz)` each expected in `0..3`, because a script names a
    /// block, not a face or a hit point — there is no screen here to have
    /// chosen one. Defaults to the CENTRE cell (`1, 1, 1`), the same cell
    /// `mod_hooks.rs`'s `centre_of` computes by hand, which is every
    /// occupied cell of a whole block and none of a Partial block whose
    /// middle happens to be air (a slab, a stair, a low crop). A script
    /// that knows which cell of a Partial block is occupied names it here;
    /// the server drops a use at an air cell exactly as it drops one out of
    /// reach — the fallback line, never a mod's `on_use`.
    Use(BlockPos, i32, i32, i32),
    /// Press a named button in a dialog this bot currently holds open, as
    /// `(qualified form, widget name, which click)`.
    ///
    /// Errors — a Lua error, so it stops the script — if the bot holds no
    /// open dialog under that form, so a script learns it pressed nothing
    /// rather than sending an event the server silently drops.
    ///
    /// One press is one message. A player's double-click is a left and then
    /// a double, and a script that means one sends both (UI ask 17).
    Press(String, String, Press),
    /// The chat lines sent to this bot since the last call, oldest first.
    /// Draining: a second call in a row with nothing new answers empty.
    Heard,
    /// The place control at whatever the bot is looking at, with nothing
    /// named — a real client's right-click at what is under its crosshair.
    ///
    /// **Life ask 18's way on**: the server casts the bot's own look ray, so
    /// a creature in front of it is used (`register_on_use_entity`, where a
    /// mod seats a rider), and with nothing on the ray it is a use at nothing,
    /// which only a `register_on_use` with `{ anywhere = true }` hears.
    UseAhead,
    /// Hold the sneak key for this many ticks, standing still — which is how
    /// a rider gets off, on a seat that lets it.
    Sneak(u64),
    /// Ask which entity the bot is riding, as the server's latest state says.
    Mounted,
    /// Close the connection.
    Disconnect,
}

/// What the async side sends back.
#[derive(Debug, Clone)]
pub enum Reply {
    /// The command succeeded and carried no data.
    Done,
    /// The current inventory, as `(material id, units)`.
    Inventory(Vec<tiamat_core::proto::StackDef>),
    /// A line of text, for a report.
    Text(String),
    /// Chat lines, oldest first — [`Command::Heard`]'s answer.
    Lines(Vec<String>),
    /// An entity, or none — [`Command::Mounted`]'s answer.
    Entity(Option<u64>),
    /// The command failed; the script should stop.
    Failed(String),
}

/// A script's channel to the bot driving it.
pub struct Channel {
    commands: mpsc::Sender<Command>,
    replies: mpsc::Receiver<Reply>,
}

impl Channel {
    /// Builds a paired channel.
    #[must_use]
    pub fn pair() -> (Self, mpsc::Receiver<Command>, mpsc::Sender<Reply>) {
        let (command_tx, command_rx) = mpsc::channel();
        let (reply_tx, reply_rx) = mpsc::channel();
        (
            Self {
                commands: command_tx,
                replies: reply_rx,
            },
            command_rx,
            reply_tx,
        )
    }

    /// Sends a command and waits for its reply.
    ///
    /// # Errors
    ///
    /// A message if the command failed or the bot went away.
    pub fn call(&self, command: Command) -> Result<Reply, String> {
        self.commands
            .send(command)
            .map_err(|_| "the bot stopped before the script finished".to_owned())?;
        match self.replies.recv() {
            Ok(Reply::Failed(message)) => Err(message),
            Ok(reply) => Ok(reply),
            Err(_) => Err("the bot stopped before replying".to_owned()),
        }
    }
}

/// How a script run ended.
#[derive(Debug)]
pub struct ScriptOutcome {
    /// Whether every assertion held.
    pub passed: bool,
    /// The failure, if there was one.
    pub failure: Option<String>,
    /// Assertions the script made.
    pub assertions: usize,
}

impl ScriptOutcome {
    /// The process exit code for this outcome.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        if self.passed { 0 } else { 1 }
    }
}

/// Strips the globals a scenario script has no business reaching.
///
/// The same list `MluaVm` denies server mods. Kept as an explicit list rather
/// than an allowlist of what stays, because Lua's standard library is small and
/// the useful half — `string`, `table`, `math` — is exactly what a scenario
/// needs.
fn remove_dangerous_globals(lua: &mlua::Lua) -> Result<(), String> {
    for global in [
        "io",
        "os",
        "package",
        "dofile",
        "loadfile",
        "load",
        "loadstring",
        "debug",
        "require",
        "ffi",
    ] {
        lua.globals()
            .set(global, mlua::Value::Nil)
            .map_err(|err| format!("could not remove `{global}` from the sandbox: {err}"))?;
    }
    Ok(())
}

/// Sends one command through a shared channel.
///
/// A poisoned lock means a previous binding panicked mid-call; reporting that
/// beats blocking forever or unwrapping into a second panic.
/// The click a script named, `"left"` when it named none.
///
/// The words a mod hears in `event.click`, so a script and the mod it drives
/// are written in one vocabulary.
fn press_named(click: Option<&str>) -> Result<Press, String> {
    match click {
        None | Some("left") => Ok(Press::Left),
        Some("right") => Ok(Press::Right),
        Some("double") => Ok(Press::Double),
        Some(other) => Err(format!(
            "bot.press: no click is called `{other}`; it is \"left\", \"right\" or \"double\""
        )),
    }
}

fn call(channel: &std::sync::Mutex<Channel>, command: Command) -> Result<Reply, String> {
    channel
        .lock()
        .map_err(|_| "the bot channel was poisoned by an earlier failure".to_owned())?
        .call(command)
}

/// Runs a Lua script against a channel, on the calling thread.
///
/// # Errors
///
/// Never returns `Err` for a failed assertion — that is a [`ScriptOutcome`]
/// with `passed: false`. The `Result` is for a script that could not be loaded
/// or a VM that could not be created.
pub fn run_script(source: &str, name: &str, channel: Channel) -> Result<ScriptOutcome, String> {
    use mlua::{Lua, Value};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // `Arc<Mutex<_>>`, not `Rc`: mlua's `send` feature — which the server
    // requires, so the whole workspace gets it — needs every closure to be
    // `Send`, and an `mpsc::Receiver` is `Send` but not `Sync`. The mutex costs
    // nothing here because the script thread issues these one at a time by
    // construction.
    let lua = Lua::new();
    remove_dangerous_globals(&lua)?;
    let channel = Arc::new(std::sync::Mutex::new(channel));
    let assertions = Arc::new(AtomicUsize::new(0));

    let table = lua
        .create_table()
        .map_err(|err| format!("could not create the bot table: {err}"))?;

    // Every binding follows the same shape: turn arguments into a `Command`,
    // block on the reply, and turn a failure into a Lua error so it unwinds to
    // the caller with a line number attached.
    macro_rules! bind {
        ($name:literal, $args:ty, |$lua:ident, $value:ident| $build:expr) => {{
            let channel = Arc::clone(&channel);
            let function = lua
                .create_function(move |$lua, $value: $args| {
                    let command: Command = $build;
                    call(&channel, command)
                        .map(|_| ())
                        .map_err(mlua::Error::external)
                })
                .map_err(|err| format!("could not bind bot.{}: {err}", $name))?;
            table
                .set($name, function)
                .map_err(|err| format!("could not set bot.{}: {err}", $name))?;
        }};
    }

    bind!("join", String, |_lua, name| Command::Join(name));
    bind!("chat", String, |_lua, text| Command::Chat(text));
    bind!("action", (String, bool), |_lua, p| Command::Action(
        p.0, p.1
    ));
    bind!("sleep_ticks", u32, |_lua, ticks| Command::SleepTicks(ticks));
    bind!("dig_block", (i32, i32, i32), |_lua, p| Command::DigBlock(
        BlockPos::new(p.0, p.1, p.2)
    ));
    bind!("dig_subnode", (i32, i32, i32), |_lua, p| {
        Command::DigSubNode(SubNodePos::new(p.0, p.1, p.2))
    });
    bind!("place", (i32, i32, i32, u16), |_lua, p| Command::Place(
        BlockPos::new(p.0, p.1, p.2),
        p.3
    ));
    bind!("place_subnode", (i32, i32, i32, u16), |_lua, p| {
        Command::PlaceSubNode(SubNodePos::new(p.0, p.1, p.2), p.3)
    });
    bind!("move_to", (f32, f32, f32), |_lua, p| Command::MoveTo(
        p.0, p.1, p.2
    ));
    bind!("expect_block", (i32, i32, i32, u16, u64), |_lua, p| {
        Command::ExpectBlock(BlockPos::new(p.0, p.1, p.2), p.3, p.4)
    });
    bind!(
        "expect_partial",
        (i32, i32, i32, u16, u32, u64),
        |_lua, p| { Command::ExpectPartial(BlockPos::new(p.0, p.1, p.2), p.3, p.4, p.5) }
    );
    bind!("expect_units", (u16, u32, u64), |_lua, p| {
        Command::ExpectUnits(p.0, p.1, p.2)
    });
    // The place control with nothing to place, at the named block's centre
    // sub-node cell unless the script names another — see `Command::Use`.
    // The trailing three are optional and independent: a script that only
    // cares about one axis of a Partial block's shape passes just that one.
    bind!(
        "use",
        (i32, i32, i32, Option<i32>, Option<i32>, Option<i32>),
        |_lua, p| Command::Use(
            BlockPos::new(p.0, p.1, p.2),
            p.3.unwrap_or(1),
            p.4.unwrap_or(1),
            p.5.unwrap_or(1),
        )
    );
    // A button, by the qualified form a `ShowDialog`/`UpdateDialog` named and
    // the widget's own name. Errors — see `Command::Press` — if the bot holds
    // no such form.
    // The click is `"left"` when left out, as every press was before a
    // press had one.
    bind!("press", (String, String, Option<String>), |_lua, p| {
        Command::Press(
            p.0,
            p.1,
            press_named(p.2.as_deref()).map_err(mlua::Error::external)?,
        )
    });
    // Sneak held for a few ticks, standing still: a rider's way off (Life
    // ask 18). Two ticks unless the script says, which is one to be sent and
    // one to be applied.
    bind!("sneak", Option<u64>, |_lua, ticks| Command::Sneak(
        ticks.unwrap_or(2)
    ));

    // `use_ahead` and `mounted` take no arguments, so they do not fit the
    // macro's shape either. The first is the place control at whatever the
    // bot looks at, the second which entity it rides — Life ask 18's way on,
    // and the question after it.
    {
        let channel = Arc::clone(&channel);
        let function = lua
            .create_function(move |_, ()| {
                call(&channel, Command::UseAhead)
                    .map(|_| ())
                    .map_err(mlua::Error::external)
            })
            .map_err(|err| format!("could not bind bot.use_ahead: {err}"))?;
        table
            .set("use_ahead", function)
            .map_err(|err| format!("could not set bot.use_ahead: {err}"))?;
    }
    {
        let channel = Arc::clone(&channel);
        let function = lua
            .create_function(move |_, ()| {
                let reply = call(&channel, Command::Mounted).map_err(mlua::Error::external)?;
                // An integer, as a mod's `game.mounted` answers, so a script
                // can compare the two; nil on foot.
                Ok(match reply {
                    Reply::Entity(entity) => entity.map(|id| id as i64),
                    _ => None,
                })
            })
            .map_err(|err| format!("could not bind bot.mounted: {err}"))?;
        table
            .set("mounted", function)
            .map_err(|err| format!("could not set bot.mounted: {err}"))?;
    }

    // `disconnect` takes no arguments, so it does not fit the macro's shape.
    {
        let channel = Arc::clone(&channel);
        let function = lua
            .create_function(move |_, ()| {
                call(&channel, Command::Disconnect)
                    .map(|_| ())
                    .map_err(mlua::Error::external)
            })
            .map_err(|err| format!("could not bind bot.disconnect: {err}"))?;
        table
            .set("disconnect", function)
            .map_err(|err| format!("could not set bot.disconnect: {err}"))?;
    }

    // `chunk_report(view)` says what the client holds within `view` chunks of
    // where the server last put it: how many positions it has in full, how
    // many as summaries (and at which levels), and the totals received. What
    // a player SEES after a teleport is this, not what the server generated.
    {
        let channel = Arc::clone(&channel);
        let function = lua
            .create_function(move |_, view: Option<i32>| {
                let reply = call(&channel, Command::ChunkReport(view.unwrap_or(6)))
                    .map_err(mlua::Error::external)?;
                Ok(match reply {
                    Reply::Text(text) => text,
                    _ => String::new(),
                })
            })
            .map_err(|err| format!("could not bind bot.chunk_report: {err}"))?;
        table
            .set("chunk_report", function)
            .map_err(|err| format!("could not set bot.chunk_report: {err}"))?;
    }

    // `inventory` returns a table of `{material = units}`, in units.
    {
        let channel = Arc::clone(&channel);
        let function = lua
            .create_function(move |lua, ()| {
                let reply = call(&channel, Command::Inventory).map_err(mlua::Error::external)?;
                let stacks = match reply {
                    Reply::Inventory(stacks) => stacks,
                    _ => Vec::new(),
                };
                let out = lua.create_table()?;
                // Keyed by material, summed across cuts. A bot script asking
                // "how much stone have I got" means the material, not one
                // particular shape of it — and a script that cares about a cut
                // is not a thing that exists yet.
                for stack in stacks {
                    let held: u32 = out.get(stack.material).unwrap_or(0);
                    out.set(stack.material, held.saturating_add(stack.units))?;
                }
                Ok(out)
            })
            .map_err(|err| format!("could not bind bot.inventory: {err}"))?;
        table
            .set("inventory", function)
            .map_err(|err| format!("could not set bot.inventory: {err}"))?;
    }

    // `heard()` returns an array table of chat lines received since the last
    // call, oldest first, and empties the buffer — Progress ask 4's third
    // call, so a script can read what a mod said back without a screen.
    {
        let channel = Arc::clone(&channel);
        let function = lua
            .create_function(move |lua, ()| {
                let reply = call(&channel, Command::Heard).map_err(mlua::Error::external)?;
                let lines = match reply {
                    Reply::Lines(lines) => lines,
                    _ => Vec::new(),
                };
                let out = lua.create_table()?;
                for (index, line) in lines.into_iter().enumerate() {
                    out.set(index + 1, line)?;
                }
                Ok(out)
            })
            .map_err(|err| format!("could not bind bot.heard: {err}"))?;
        table
            .set("heard", function)
            .map_err(|err| format!("could not set bot.heard: {err}"))?;
    }

    // `assert` counts as well as checks, so a script that silently asserted
    // nothing is distinguishable from one that passed.
    {
        let assertions = Arc::clone(&assertions);
        let function = lua
            .create_function(move |_, (condition, message): (Value, Option<String>)| {
                assertions.fetch_add(1, Ordering::Relaxed);
                let truthy = !matches!(condition, Value::Nil | Value::Boolean(false));
                if truthy {
                    Ok(())
                } else {
                    Err(mlua::Error::external(
                        message.unwrap_or_else(|| "assertion failed".to_owned()),
                    ))
                }
            })
            .map_err(|err| format!("could not bind bot.assert: {err}"))?;
        table
            .set("assert", function)
            .map_err(|err| format!("could not set bot.assert: {err}"))?;
    }

    // Constants a script needs to do unit arithmetic without hard-coding 27.
    table
        .set("UNITS_PER_BLOCK", tiamat_core::UNITS_PER_BLOCK)
        .map_err(|err| format!("could not set bot.UNITS_PER_BLOCK: {err}"))?;
    table
        .set("AIR", tiamat_core::MaterialId::AIR.0)
        .map_err(|err| format!("could not set bot.AIR: {err}"))?;

    lua.globals()
        .set("bot", table)
        .map_err(|err| format!("could not install the bot table: {err}"))?;

    let result = lua.load(source).set_name(format!("@{name}")).exec();
    let assertions = assertions.load(Ordering::Relaxed);

    match result {
        Ok(()) => Ok(ScriptOutcome {
            passed: true,
            failure: None,
            assertions,
        }),
        Err(err) => Ok(ScriptOutcome {
            passed: false,
            // `to_string` on an mlua error includes the chunk name and line,
            // which is the whole reason scripts are worth having.
            failure: Some(err.to_string()),
            assertions,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs a script with a stub bot that always succeeds, and returns both the
    /// outcome and the commands the script issued.
    fn run_with_stub(source: &str) -> (ScriptOutcome, Vec<Command>) {
        run_with_replies(source, |_| Reply::Done)
    }

    /// Runs a script with a stub that answers each command however `answer`
    /// says.
    fn run_with_replies(
        source: &str,
        answer: impl Fn(&Command) -> Reply + Send + 'static,
    ) -> (ScriptOutcome, Vec<Command>) {
        let (channel, commands, replies) = Channel::pair();
        let stub = std::thread::spawn(move || {
            let mut seen = Vec::new();
            while let Ok(command) = commands.recv() {
                let reply = answer(&command);
                seen.push(command);
                if replies.send(reply).is_err() {
                    break;
                }
            }
            seen
        });

        let outcome = run_script(source, "test", channel).expect("the VM should start");
        // Dropping the channel ends the stub's loop.
        let seen = stub.join().expect("stub thread");
        (outcome, seen)
    }

    #[test]
    fn a_script_presses_a_mods_action_by_name() {
        // **The converged pathway.** A scenario presses the same named action a
        // player presses, so it exercises the human path rather than a parallel
        // one that can rot without anybody noticing.
        //
        // By ID and not by key, for the reason charter rule 11 gives mods names
        // instead of keys: a script written against a key would break the
        // moment somebody rebound it, and rebinding must change controls
        // without changing behaviour.
        let (outcome, commands) = run_with_stub(
            "bot.join('Alice')\n\
             bot.action('core_tools:chisel_mode', true)\n\
             bot.dig_subnode(1, 2, 3)\n\
             bot.action('core_tools:chisel_mode', false)\n\
             bot.disconnect()",
        );

        assert!(outcome.passed, "{:?}", outcome.failure);
        assert_eq!(
            commands,
            vec![
                Command::Join("Alice".to_owned()),
                Command::Action("core_tools:chisel_mode".to_owned(), true),
                Command::DigSubNode(SubNodePos::new(1, 2, 3)),
                Command::Action("core_tools:chisel_mode".to_owned(), false),
                Command::Disconnect,
            ]
        );
    }

    #[test]
    fn a_script_issues_the_commands_it_asks_for() {
        let (outcome, commands) = run_with_stub(
            "bot.join('Alice')\nbot.dig_block(1, 2, 3)\nbot.place(4, 5, 6, 7)\nbot.disconnect()",
        );

        assert!(outcome.passed, "{:?}", outcome.failure);
        assert_eq!(
            commands,
            vec![
                Command::Join("Alice".to_owned()),
                Command::DigBlock(BlockPos::new(1, 2, 3)),
                Command::Place(BlockPos::new(4, 5, 6), 7),
                Command::Disconnect,
            ]
        );
    }

    #[test]
    fn a_failing_assertion_fails_the_run_and_names_the_line() {
        // The whole reason for scripts over hand-written clients: a failure
        // points at a line a human wrote.
        let (outcome, _) = run_with_stub("bot.assert(false, 'the world was wrong')");

        assert!(!outcome.passed);
        assert_eq!(outcome.exit_code(), 1);
        let failure = outcome.failure.expect("a failure message");
        assert!(
            failure.contains("the world was wrong"),
            "the message must survive: {failure}"
        );
        assert!(
            failure.contains("test"),
            "the chunk name should locate it: {failure}"
        );
    }

    #[test]
    fn a_passing_assertion_is_counted() {
        // A script that asserted nothing must be distinguishable from one that
        // passed, or an empty script looks like a green test.
        let (outcome, _) = run_with_stub("bot.assert(true)\nbot.assert(1 == 1)");
        assert!(outcome.passed);
        assert_eq!(outcome.assertions, 2);

        let (empty, _) = run_with_stub("-- does nothing");
        assert!(empty.passed);
        assert_eq!(empty.assertions, 0, "an empty script asserts nothing");
    }

    #[test]
    fn a_command_failure_stops_the_script() {
        // A bot that could not dig must not let the script carry on asserting
        // about a world it never changed.
        let (outcome, commands) = run_with_replies(
            "bot.join('Alice')\nbot.dig_block(0, 0, 0)\nbot.chat('never reached')",
            |command| match command {
                Command::DigBlock(_) => Reply::Failed("the server refused".to_owned()),
                _ => Reply::Done,
            },
        );

        assert!(!outcome.passed);
        assert!(
            outcome
                .failure
                .as_ref()
                .is_some_and(|f| f.contains("the server refused")),
            "{:?}",
            outcome.failure
        );
        assert!(
            !commands.contains(&Command::Chat("never reached".to_owned())),
            "the script must stop at the failure"
        );
    }

    #[test]
    fn inventory_comes_back_as_a_table_in_units() {
        // Charter rule 5: scripts do unit arithmetic, so the constant is
        // exposed rather than written as 27 in every script.
        let (outcome, _) = run_with_replies(
            "local inv = bot.inventory()\n\
             bot.assert(inv[2] == 243, 'expected 243 units, got ' .. tostring(inv[2]))\n\
             bot.assert(inv[2] / bot.UNITS_PER_BLOCK == 9, 'expected 9 blocks')",
            |command| match command {
                Command::Inventory => Reply::Inventory(vec![tiamat_core::proto::StackDef {
                    material: 2,
                    units: 243,
                    shape: 0,
                    detail: None,
                    cells: Vec::new(),
                }]),
                _ => Reply::Done,
            },
        );
        assert!(outcome.passed, "{:?}", outcome.failure);
        assert_eq!(outcome.assertions, 2);
    }

    #[test]
    fn a_lua_syntax_error_is_a_failure_not_a_panic() {
        let (outcome, _) = run_with_stub("this is not lua ((((");
        assert!(!outcome.passed);
        assert!(outcome.failure.is_some());
    }

    #[test]
    fn a_script_cannot_reach_the_filesystem() {
        // Enumerated rather than trusted. The module docs claimed this sandbox
        // existed while `run_script` built a bare `Lua` and `io.open` worked;
        // this test is what found it.
        for source in [
            "local f = io.open('/etc/passwd')",
            "os.execute('rm -rf /')",
            "local d = require('os')",
            "dofile('/etc/passwd')",
            "loadfile('/etc/passwd')",
            "local f = load('return 1')",
            "debug.getinfo(1)",
            "local p = package.path",
        ] {
            let (outcome, _) = run_with_stub(source);
            assert!(
                !outcome.passed,
                "`{source}` should not have been allowed to run"
            );
        }
    }

    #[test]
    fn the_useful_half_of_the_standard_library_survives() {
        // A sandbox that removed `string` and `math` would make scenario
        // scripts unwritable, which is a different way of being useless.
        let (outcome, _) = run_with_stub(
            "bot.assert(string.format('%d', 27) == '27')\n\
             bot.assert(math.floor(3.7) == 3)\n\
             bot.assert(#{1, 2, 3} == 3)\n\
             bot.assert(table.concat({'a', 'b'}, '-') == 'a-b')",
        );
        assert!(outcome.passed, "{:?}", outcome.failure);
        assert_eq!(outcome.assertions, 4);
    }

    #[test]
    fn expect_block_carries_its_timeout() {
        let (outcome, commands) = run_with_stub("bot.expect_block(1, 2, 3, 9, 5000)");
        assert!(outcome.passed, "{:?}", outcome.failure);
        assert_eq!(
            commands,
            vec![Command::ExpectBlock(BlockPos::new(1, 2, 3), 9, 5000)]
        );
    }

    #[test]
    fn a_ride_is_got_on_ahead_got_off_by_sneaking_and_asked_after() {
        // Life ask 18's three verbs: the use at what is ahead that a mod
        // seats a rider from, the sneak that gets them off, and the question
        // whose answer is the mount's id as `game.mounted` gives it, or nil.
        let riding = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let state = std::sync::Arc::clone(&riding);
        let (outcome, seen) = run_with_replies(
            "bot.join('Alice')\n\
             bot.use_ahead()\n\
             bot.assert(bot.mounted() == 12, 'mounted says ' .. tostring(bot.mounted()))\n\
             bot.sneak()\n\
             bot.sneak(5)\n\
             bot.assert(bot.mounted() == nil, 'still mounted')",
            move |command| {
                use std::sync::atomic::Ordering;
                match command {
                    Command::UseAhead => state.store(true, Ordering::Relaxed),
                    Command::Sneak(_) => state.store(false, Ordering::Relaxed),
                    Command::Mounted => {
                        return Reply::Entity(state.load(Ordering::Relaxed).then_some(12));
                    }
                    _ => {}
                }
                Reply::Done
            },
        );
        assert!(outcome.passed, "{:?}", outcome.failure);
        assert_eq!(outcome.assertions, 2);
        assert!(!riding.load(std::sync::atomic::Ordering::Relaxed));
        assert_eq!(
            seen,
            vec![
                Command::Join("Alice".to_owned()),
                Command::UseAhead,
                // Twice: Lua builds the assertion's message before the call.
                Command::Mounted,
                Command::Mounted,
                Command::Sneak(2),
                Command::Sneak(5),
                Command::Mounted,
            ]
        );
    }

    #[test]
    fn a_use_names_the_block_and_defaults_to_its_centre_cell() {
        // Progress ask 4: `bot.use(x, y, z)` takes a whole block, because a
        // script has no screen to have picked a face or a hit point with.
        // With no offset given the command still carries the centre cell —
        // `runner.rs`'s job is only to hand it to `use_block` unchanged.
        let (outcome, commands) = run_with_stub("bot.join('Alice')\nbot.use(2, -1, 0)");
        assert!(outcome.passed, "{:?}", outcome.failure);
        assert_eq!(
            commands,
            vec![
                Command::Join("Alice".to_owned()),
                Command::Use(BlockPos::new(2, -1, 0), 1, 1, 1),
            ]
        );
    }

    #[test]
    fn a_use_can_name_an_occupied_cell_of_a_partial_block() {
        // The centre cell is air on some Partial shapes (a slab, a stair, a
        // low crop) — a script that knows which cell is occupied names it,
        // rather than the server dropping the use for aiming at air.
        let (outcome, commands) = run_with_stub("bot.join('Alice')\nbot.use(2, -1, 0, 0, 0, 2)");
        assert!(outcome.passed, "{:?}", outcome.failure);
        assert_eq!(
            commands,
            vec![
                Command::Join("Alice".to_owned()),
                Command::Use(BlockPos::new(2, -1, 0), 0, 0, 2),
            ]
        );
    }

    #[test]
    fn a_use_offset_is_a_non_numeric_argument_error() {
        // A script that passes something that is not a cell offset learns
        // that at the call, as a script-stopping error — not a use silently
        // aimed at the wrong cell.
        let (outcome, _) = run_with_stub("bot.use(2, -1, 0, 'nowhere', 0, 0)");
        assert!(!outcome.passed);
        assert!(
            outcome.failure.is_some(),
            "a non-numeric offset must fail the script"
        );
    }

    #[test]
    fn a_press_names_the_form_and_the_button() {
        let (outcome, commands) = run_with_stub("bot.press('warden:panel', 'go')");
        assert!(outcome.passed, "{:?}", outcome.failure);
        assert_eq!(
            commands,
            vec![Command::Press(
                "warden:panel".to_owned(),
                "go".to_owned(),
                Press::Left
            )]
        );
    }

    #[test]
    fn a_press_may_say_which_click_it_was() {
        // UI ask 17: what a player's right-click and double-click send, and
        // a double-click is the left it began as and then the double.
        let (outcome, commands) = run_with_stub(
            "bot.press('ui:screen', 'row', 'right')
             bot.press('ui:screen', 'row', 'left')
             bot.press('ui:screen', 'row', 'double')",
        );
        assert!(outcome.passed, "{:?}", outcome.failure);
        let press = |click| Command::Press("ui:screen".to_owned(), "row".to_owned(), click);
        assert_eq!(
            commands,
            vec![
                press(Press::Right),
                press(Press::Left),
                press(Press::Double)
            ]
        );
    }

    #[test]
    fn a_click_nobody_makes_fails_the_script() {
        // Not read as a left: a script that misspells `double` would
        // otherwise pass while testing something else.
        let (outcome, commands) = run_with_stub("bot.press('ui:screen', 'row', 'middle')");
        assert!(!outcome.passed);
        assert!(commands.is_empty(), "nothing is sent: {commands:?}");
        let failure = outcome.failure.expect("a failure message");
        assert!(
            failure.contains("`middle`") && failure.contains("\"double\""),
            "the click and the choices must be named: {failure}"
        );
    }

    #[test]
    fn pressing_a_form_the_bot_does_not_hold_fails_the_script() {
        // The bot side of Progress ask 4's error requirement: whatever
        // decided the bot holds no such form, the script must stop and say
        // so rather than carrying on as if the button had been pressed.
        let (outcome, _) =
            run_with_replies("bot.press('warden:nope', 'go')", |command| match command {
                Command::Press(form, ..) => {
                    Reply::Failed(format!("the bot holds no open dialog for form `{form}`"))
                }
                _ => Reply::Done,
            });
        assert!(!outcome.passed);
        let failure = outcome.failure.expect("a failure message");
        // Not merely `contains("warden:nope")` — `Command::Press`'s own
        // `Debug` also contains the form name, so that alone would pass for
        // any failure of the command whatsoever. Assert the actual
        // `NoSuchForm` text a real bot produces, so this proves the message
        // itself survives rather than that some failure occurred at all.
        assert!(
            failure.contains("holds no open dialog for form `warden:nope`"),
            "the form must be named: {failure}"
        );
    }

    #[test]
    fn heard_returns_the_lines_in_order_oldest_first() {
        let (outcome, _) = run_with_replies(
            "local lines = bot.heard()\n\
             bot.assert(#lines == 2, 'expected 2 lines, got ' .. tostring(#lines))\n\
             bot.assert(lines[1] == 'first', lines[1])\n\
             bot.assert(lines[2] == 'second', lines[2])",
            |command| match command {
                Command::Heard => Reply::Lines(vec!["first".to_owned(), "second".to_owned()]),
                _ => Reply::Done,
            },
        );
        assert!(outcome.passed, "{:?}", outcome.failure);
        assert_eq!(outcome.assertions, 3);
    }

    #[test]
    fn heard_with_nothing_new_is_an_empty_table() {
        let (outcome, _) = run_with_replies(
            "bot.assert(#bot.heard() == 0, 'expected nothing heard')",
            |command| match command {
                Command::Heard => Reply::Lines(Vec::new()),
                _ => Reply::Done,
            },
        );
        assert!(outcome.passed, "{:?}", outcome.failure);
    }
}
