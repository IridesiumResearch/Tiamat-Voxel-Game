// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The scripting runtime, behind one trait.
//!
//! # Why a trait at all, when there is one backend
//!
//! [`ScriptVm`] is the only path into a script VM. Nothing outside this module
//! names a backend type, and CI greps to keep it that way.
//!
//! That is not architecture for its own sake. The backend is chosen by
//! measurement (see `docs/scripting-vm.md`) and the choice is **irreversible in
//! practice** — mod-visible language semantics differ between the candidates, so
//! switching later breaks every mod ever written. The trait cannot undo that.
//! What it does buy is:
//!
//! - the *benchmark* that makes the decision, which needs all three behind one
//!   API to compare them at all;
//! - a future WASM tier, which would be an addition rather than a rewrite;
//! - a boundary where budgets, memory limits, and crash isolation are enforced
//!   once instead of at every call site.
//!
//! # The rule scripts cannot break by accident
//!
//! **Script code must not do simulation-relevant float maths.** Charter rule 4's
//! Deterministic Float Subset cannot be enforced inside a script VM — Lua has
//! one number type and no lint reaches into it, and `x^0.5` in a mod is a libm
//! call on whatever platform the server happens to run.
//!
//! So worldgen scripts **orchestrate** native fills; they never compute
//! per-sample values. The API is shaped to make that the only ergonomic path: a
//! generator asks for a heightmap and hands it to a fill, and there is no
//! exposed way to loop over samples in Lua. Making the wrong thing awkward is
//! more reliable than documenting that it is wrong.

mod host;
mod vm;

pub use host::{HostError, ModHost, Phase, read_manifest};
pub use vm::{
    AbsorbsRule, ActionEvent, Backend, BlockRules, BlockTexture, Brush, ChatEvent, DialogEvent,
    DigEvent, DismountEvent, DomainEvent, FluidFlowEvent, FluidRules, HookOutcome, JoinEvent,
    LeaveEvent, MAX_REFUSAL_BYTES, MoveEvent, PlaceEvent, PunchEvent, RandomTickEvent, ScriptError,
    ScriptVm, Sky, SkyGrade, SkyKeyframe, Tool, UseAim, UseEntityEvent, UseEvent, VmLimits,
    WorldEdit, resolve_tool_speeds,
};

/// The version of the mod API, `major.minor`, separate from the engine's.
///
/// `1.0` is the API as frozen at engine 0.5.0. A mod reads it as
/// `game.api_version`; the header of `api/stubs/game.lua` names the version
/// the stubs describe, and `scripts/check-stubs.sh` fails CI if the two
/// differ. The rule the number carries, written for mod authors in
/// `api/AGENTS.md`: within a major, nothing a mod can call goes away or
/// changes meaning without a deprecation first — a `---@deprecated` line on
/// the stub naming the minor it was deprecated in, the minor it goes in and
/// what to use instead — and at least one minor release in which it still
/// works as it did. Additions raise the minor. Raising the major is the
/// deliberate breaking of that rule, and none is planned.
pub const API_VERSION: &str = "1.0";

#[cfg(feature = "script")]
mod budget;

#[cfg(feature = "script")]
mod hud_vm;

#[cfg(feature = "script")]
mod mlua_vm;

#[cfg(feature = "script")]
pub use hud_vm::{Fault, HudLimits, HudVm};

#[cfg(feature = "script")]
pub use mlua_vm::MluaVm;

/// The VM the engine uses.
///
/// One alias so callers name a concrete type without naming a backend.
#[cfg(feature = "script")]
pub type EngineVm = MluaVm;

/// The engine's mod host, over the chosen VM.
#[cfg(feature = "script")]
pub type EngineHost = ModHost<MluaVm>;
