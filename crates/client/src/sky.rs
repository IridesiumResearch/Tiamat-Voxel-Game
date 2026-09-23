// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The sky a mod described, and where it stands right now.
//!
//! # The engine interpolates; the mod decides what between
//!
//! Charter rule 1. Everything here is arithmetic over a list of colours the
//! client was handed: how long a day is, what colour dawn goes, whether there
//! is a day at all — none of it is known here. A world whose mods register no
//! sky gets [`Sky::none`], which never changes, and that is a legitimate world
//! rather than a missing feature.
//!
//! # Why the client interpolates rather than the server sending a colour
//!
//! The colour changes every frame and the tick runs at 20 Hz. Sending a colour
//! would either look stepped or cost a message per frame per player, and the
//! interpolation is four multiplications — presentation work, on the machine
//! doing the presenting. What the server owns is the *clock*, because two
//! players standing together must see the same sky.

use tiamat_core::atmosphere::{CloudMap, Clouds, Flash, SkyModifier};
use tiamat_core::proto::{SkyFrame, SkyGrade};

/// Everything a mod's weather does to this player's sky: the standing
/// modifier on its way to where the mod put it, and the flashes lighting it
/// right now.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Weather {
    /// The modifier, easing.
    pub modifier: Eased,
    /// Lightning, seen.
    pub flashes: Flashes,
    /// The rain around this client, spawned here from the shape the server
    /// sent.
    pub rain: crate::particles::Emitter,
    /// How much cloud this player is under, easing — weather ask W18.
    pub deck: EasedDeck,
    /// The coarse cover map, easing cell by cell — weather ask W18.
    pub map: EasedMap,
}

/// A clear sky over a plain deck: what a world is under until a mod says,
/// and what a deck fades to when the weather is called off.
pub const CLEAR: Clouds = Clouds {
    cover: 0.0,
    darkness: 0.0,
    base: None,
    ease_ticks: 0,
    stratocumulus: 0.0,
    altocumulus: 0.0,
    cumulonimbus: 0.0,
};

/// The deck's state on its way to where the server put it — weather ask
/// W18.
///
/// `Clouds::ease_ticks` was carried in the message and read by nothing: a
/// change of cover, darkness, genus or floor was a step on the client. Now
/// the state blends from wherever it had got to over the ticks the new one
/// names, as the sky modifier does — and, as there, "back to clear" takes as
/// long as the weather took to arrive.
#[derive(Debug, Clone, PartialEq)]
pub struct EasedDeck {
    from: Clouds,
    to: Clouds,
    /// What the server last said, verbatim: `None` is no state at all.
    target: Option<Clouds>,
    elapsed: f32,
    duration: f32,
}

impl Default for EasedDeck {
    fn default() -> Self {
        Self {
            from: CLEAR,
            to: CLEAR,
            target: None,
            elapsed: 0.0,
            duration: 0.0,
        }
    }
}

impl EasedDeck {
    /// Sets where to go, eased over the target's `ease_ticks` — or, for a
    /// clearing, over the ticks the last state had.
    pub fn set(&mut self, target: Option<Clouds>) {
        let ticks = target.map_or(self.to.ease_ticks, |target| target.ease_ticks);
        self.from = self.current().unwrap_or(CLEAR);
        self.to = target.unwrap_or(CLEAR);
        self.target = target;
        self.elapsed = 0.0;
        self.duration = tiamat_core::tick::TICK_DURATION.as_secs_f32() * ticks as f32;
    }

    /// Advances by a frame.
    pub fn advance(&mut self, dt: f32) {
        self.elapsed += dt.max(0.0);
    }

    /// What the server last said, for the tests that check it arrived.
    #[must_use]
    pub const fn target(&self) -> Option<Clouds> {
        self.target
    }

    /// The ticks the state now eases over, which a map arriving beside it
    /// eases over too.
    #[must_use]
    pub const fn ease_ticks(&self) -> u32 {
        self.to.ease_ticks
    }

    fn arrived(&self) -> bool {
        self.duration <= 0.0 || self.elapsed >= self.duration
    }

    /// Where the deck stands now: exactly the target once arrived, and
    /// `None` only when there is no state at all and nothing to fade from.
    #[must_use]
    pub fn current(&self) -> Option<Clouds> {
        if self.arrived() {
            return self.target;
        }
        let blend = (self.elapsed / self.duration).clamp(0.0, 1.0);
        let scalar = |from: f32, to: f32| from + (to - from) * blend;
        Some(Clouds {
            cover: scalar(self.from.cover, self.to.cover),
            darkness: scalar(self.from.darkness, self.to.darkness),
            // A floor eases between two floors; a floor appearing or going
            // has nothing to ease from, and steps.
            base: match (self.from.base, self.to.base) {
                (Some(from), Some(to)) => Some(scalar(from, to)),
                _ => self.to.base,
            },
            ease_ticks: self.to.ease_ticks,
            stratocumulus: scalar(self.from.stratocumulus, self.to.stratocumulus),
            altocumulus: scalar(self.from.altocumulus, self.to.altocumulus),
            cumulonimbus: scalar(self.from.cumulonimbus, self.to.cumulonimbus),
        })
    }
}

/// The cover map on its way to the one the server last sent — weather ask
/// W18.
///
/// Cell by cell, over the ticks the deck's own state eases over: a map
/// re-sent with one cell darker does not step that cell in one frame. Two
/// maps on the same grid blend; a map arriving where there was none blends
/// up from the plain state, and one going away blends down to it; a map on
/// a different grid — moved, or resized — steps, since its cells are not
/// the old ones.
#[derive(Debug, Clone, PartialEq)]
pub struct EasedMap {
    from: Option<std::sync::Arc<CloudMap>>,
    to: Option<std::sync::Arc<CloudMap>>,
    /// The plain state a map arriving where there was none fades up from.
    plain_from: Clouds,
    elapsed: f32,
    duration: f32,
}

impl Default for EasedMap {
    fn default() -> Self {
        Self {
            from: None,
            to: None,
            plain_from: CLEAR,
            elapsed: 0.0,
            duration: 0.0,
        }
    }
}

impl EasedMap {
    /// Sets the map to go to, eased over `ticks`; `plain` is the single
    /// state the deck stands at now, which a map fades in from or out to.
    pub fn set(&mut self, target: Option<std::sync::Arc<CloudMap>>, ticks: u32, plain: &Clouds) {
        self.from = self.current(plain);
        self.plain_from = *plain;
        self.to = target;
        self.elapsed = 0.0;
        self.duration = tiamat_core::tick::TICK_DURATION.as_secs_f32() * ticks as f32;
    }

    /// Advances by a frame.
    pub fn advance(&mut self, dt: f32) {
        self.elapsed += dt.max(0.0);
    }

    /// What the server last sent, for the tests that check it arrived.
    #[must_use]
    pub fn target(&self) -> Option<&CloudMap> {
        self.to.as_deref()
    }

    fn arrived(&self) -> bool {
        self.duration <= 0.0 || self.elapsed >= self.duration
    }

    /// The map to draw now: exactly the target once arrived, a blend on the
    /// way, and the target at once when the grids differ.
    #[must_use]
    pub fn current(&self, plain: &Clouds) -> Option<std::sync::Arc<CloudMap>> {
        if self.arrived() {
            return self.to.clone();
        }
        let blend = (self.elapsed / self.duration).clamp(0.0, 1.0);
        match (&self.from, &self.to) {
            (Some(from), Some(to)) if same_grid(from, to) => {
                Some(std::sync::Arc::new(blend_maps(from, to, blend)))
            }
            (None, Some(to)) => {
                let from = filled_like(to, &self.plain_from);
                Some(std::sync::Arc::new(blend_maps(&from, to, blend)))
            }
            (Some(from), None) => {
                let to = filled_like(from, plain);
                Some(std::sync::Arc::new(blend_maps(from, &to, blend)))
            }
            _ => self.to.clone(),
        }
    }
}

/// Whether two maps' cells are the same cells: the same corner, size and
/// side, bit for bit — a grid that moved by a hair is another grid.
fn same_grid(a: &CloudMap, b: &CloudMap) -> bool {
    a.size == b.size
        && a.origin[0].to_bits() == b.origin[0].to_bits()
        && a.origin[1].to_bits() == b.origin[1].to_bits()
        && a.cell.to_bits() == b.cell.to_bits()
}

/// A map on `like`'s grid holding the plain state in every cell: what a
/// grid looked like before it arrived, or will once it has gone.
fn filled_like(like: &CloudMap, plain: &Clouds) -> CloudMap {
    let count = usize::from(like.size) * usize::from(like.size);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a unit share to a byte"
    )]
    let byte = |share: f32| (share.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    CloudMap {
        origin: like.origin,
        cell: like.cell,
        size: like.size,
        cover: vec![byte(plain.cover); count],
        darkness: vec![byte(plain.darkness); count],
        stratocumulus: vec![byte(plain.stratocumulus); count],
        altocumulus: vec![byte(plain.altocumulus); count],
        cumulonimbus: vec![byte(plain.cumulonimbus); count],
    }
}

/// Two maps on one grid, `blend` of the way from the first to the second.
/// A genus left out of one is none anywhere in it, and blends from that.
fn blend_maps(from: &CloudMap, to: &CloudMap, blend: f32) -> CloudMap {
    let count = usize::from(to.size) * usize::from(to.size);
    let cells = |a: &[u8], b: &[u8]| -> Vec<u8> {
        if a.is_empty() && b.is_empty() {
            return Vec::new();
        }
        (0..count)
            .map(|index| {
                let (from, to) = (
                    f32::from(a.get(index).copied().unwrap_or(0)),
                    f32::from(b.get(index).copied().unwrap_or(0)),
                );
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "between two bytes"
                )]
                {
                    (from + (to - from) * blend + 0.5) as u8
                }
            })
            .collect()
    };
    CloudMap {
        origin: to.origin,
        cell: to.cell,
        size: to.size,
        cover: cells(&from.cover, &to.cover),
        darkness: cells(&from.darkness, &to.darkness),
        stratocumulus: cells(&from.stratocumulus, &to.stratocumulus),
        altocumulus: cells(&from.altocumulus, &to.altocumulus),
        cumulonimbus: cells(&from.cumulonimbus, &to.cumulonimbus),
    }
}

/// A mod's sky modifier on its way to where the mod put it.
///
/// **The server sends a target and a time; the client fills the gap**, as
/// it does for the keyframes. A modifier set while another is still easing
/// starts from wherever that one had got to, so a front that arrives and is
/// then called off never snaps.
#[derive(Debug, Clone, PartialEq)]
pub struct Eased {
    from: SkyModifier,
    to: SkyModifier,
    /// Seconds since `to` was set.
    elapsed: f32,
    /// Seconds the move takes; zero is at once.
    duration: f32,
}

impl Default for Eased {
    fn default() -> Self {
        Self::none()
    }
}

impl Eased {
    /// The plain sky, going nowhere.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            from: SkyModifier::NONE,
            to: SkyModifier::NONE,
            elapsed: 0.0,
            duration: 0.0,
        }
    }

    /// Sets where to go: a modifier, or `None` for the plain sky, eased over
    /// the ticks the LAST modifier had — "back to normal" takes as long as
    /// the storm took to arrive.
    pub fn set(&mut self, target: Option<SkyModifier>) {
        let ticks = target.map_or(self.to.ease_ticks, |target| target.ease_ticks);
        self.from = self.current();
        self.to = target.unwrap_or(SkyModifier::NONE);
        self.elapsed = 0.0;
        self.duration = tiamat_core::tick::TICK_DURATION.as_secs_f32() * ticks as f32;
    }

    /// Advances by a frame.
    pub fn advance(&mut self, dt: f32) {
        self.elapsed += dt.max(0.0);
    }

    /// Where the modifier stands now.
    ///
    /// Exactly `to` once arrived — not nearly: a modifier at its identity
    /// must leave the sky bit-identical, or every ungraded world would start
    /// paying for a grading LUT (see [`SkyGrade::is_none`]).
    #[must_use]
    pub fn current(&self) -> SkyModifier {
        if self.duration <= 0.0 || self.elapsed >= self.duration {
            return self.to;
        }
        let blend = self.elapsed / self.duration;
        let scalar = |from: f32, to: f32| from + (to - from) * blend;
        SkyModifier {
            intensity: scalar(self.from.intensity, self.to.intensity),
            sky: mix(self.from.sky, self.to.sky, blend),
            sky_mix: scalar(self.from.sky_mix, self.to.sky_mix),
            fog_distance: scalar(self.from.fog_distance, self.to.fog_distance),
            saturation: scalar(self.from.saturation, self.to.saturation),
            ease_ticks: self.to.ease_ticks,
        }
    }
}

/// The flashes lighting this player's sky right now.
///
/// Each runs an envelope — up over its attack, down over its decay — and
/// they add, capped where the renderer caps the sun. Presentation, on frame
/// time.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Flashes {
    /// Each with its age in seconds.
    active: Vec<(Flash, f32)>,
}

impl Flashes {
    /// A strike, just now.
    pub fn strike(&mut self, flash: Flash) {
        if self.active.len() < MAX_FLASHES {
            self.active.push((flash, 0.0));
        }
    }

    /// Advances by a frame and forgets what has died away.
    pub fn advance(&mut self, dt: f32) {
        let tick = tiamat_core::tick::TICK_DURATION.as_secs_f32();
        for (_, age) in &mut self.active {
            *age += dt.max(0.0);
        }
        self.active
            .retain(|(flash, age)| *age <= (flash.attack_ticks + flash.decay_ticks) as f32 * tick);
    }

    /// How much light there is now, and its colour: the sum of every
    /// envelope, and the colour of the brightest.
    #[must_use]
    pub fn light(&self) -> (f32, [f32; 3]) {
        let tick = tiamat_core::tick::TICK_DURATION.as_secs_f32();
        let mut total = 0.0_f32;
        let mut brightest = (0.0_f32, [1.0; 3]);
        for (flash, age) in &self.active {
            let attack = flash.attack_ticks as f32 * tick;
            let decay = flash.decay_ticks as f32 * tick;
            let envelope = if *age < attack {
                *age / attack
            } else if decay > 0.0 {
                (1.0 - (*age - attack) / decay).max(0.0)
            } else {
                0.0
            };
            let amount = flash.intensity * envelope;
            total += amount;
            if amount > brightest.0 {
                brightest = (amount, flash.colour);
            }
        }
        (total, brightest.1)
    }

    /// Whether anything is lit.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.active.is_empty()
    }
}

/// How many strikes a client will hold at once; more than this is one storm.
const MAX_FLASHES: usize = 16;

/// A moment with the flashes added.
///
/// The sun's intensity gains the light (the renderer caps it at daylight),
/// and the sun and sky lean towards the flash's colour by it. With no light
/// every term is an add of zero or a lerp by zero, which is exact.
#[must_use]
pub fn flashed(moment: Moment, flashes: &Flashes) -> Moment {
    if flashes.is_empty() {
        return moment;
    }
    let (amount, colour) = flashes.light();
    let lean = amount.min(1.0);
    Moment {
        intensity: moment.intensity + amount,
        sun: mix(moment.sun, colour, lean),
        sky: mix(moment.sky, colour, lean * 0.5),
        ..moment
    }
}

/// A moment with a mod's modifier laid over it.
///
/// Every term is a multiply or a lerp whose identity leaves the value
/// exactly as it was: `x * 1.0`, `x + (y - x) * 0.0`. The grade is touched
/// only when the modifier says something about it, so [`SkyGrade::NONE`]
/// stays `NONE` under a modifier that has nothing to say — which is what
/// keeps an ungraded world off the LUT.
#[must_use]
#[expect(clippy::float_cmp, reason = "an identity is exact or it is not one")]
pub fn modified(moment: Moment, modifier: &SkyModifier) -> Moment {
    let mut grade = moment.grade;
    if modifier.saturation != 1.0 {
        grade.saturation = (grade.saturation * modifier.saturation).clamp(0.0, GRADE_MAX);
    }
    Moment {
        sky: mix(moment.sky, modifier.sky, modifier.sky_mix),
        intensity: moment.intensity * modifier.intensity,
        grade,
        ..moment
    }
}

/// A sky's colours, and the clock that walks them.
#[derive(Debug, Clone, PartialEq)]
pub struct Sky {
    /// Ticks in a full day. Zero means no mod registered a sky.
    day_length_ticks: u32,
    /// Keyframes, sorted by time.
    keyframes: Vec<SkyFrame>,
    /// Where the day stands, `0.0..1.0`.
    time: f32,
    /// Where in the universe this sky is seen from — the domain's place,
    /// which is what the star catalog is drawn from.
    observer: tiamat_core::sky::UniversalPos,
}

/// What the sky looks like at one moment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Moment {
    /// The sky's own colour, which distance fog fades towards.
    pub sky: [f32; 3],
    /// The sun's colour.
    pub sun: [f32; 3],
    /// How strong the sun is, `0.0..=1.0`.
    pub intensity: f32,
    /// Which way the sunlight travels, normalised — from the sun towards the
    /// world, so a surface facing `-direction` is the one facing the sun.
    ///
    /// Shadow maps need this and colour alone cannot supply it. See
    /// [`Sky::sun_direction`] for the arc it walks and what is fixed about it.
    pub sun_direction: [f32; 3],
    /// How much of the star catalog shows, `0.0..=1.0`. Zero is none, and
    /// is what a sky that never mentioned stars gets.
    pub stars: f32,
    /// How the finished picture is graded now.
    ///
    /// Interpolated between keyframes like the colours are, and **sanitised** on
    /// the way through: this arrives from a peer, and a grade that came in as
    /// `NaN` would leave the whole frame one colour. See
    /// [`crate::render::grade`].
    pub grade: SkyGrade,
}

impl Sky {
    /// A world with no sky mod: one fixed daylight moment, for ever.
    ///
    /// **Not an error case.** The engine registers no sky (charter rule 1), so
    /// this is what a world without one legitimately looks like — and it is
    /// exactly the lighting Task 08's scenes were built against, which is why
    /// their screenshots still hold.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            day_length_ticks: 0,
            keyframes: Vec::new(),
            time: 0.0,
            observer: tiamat_core::sky::UniversalPos::CENTRE,
        }
    }

    /// The sky a server described.
    #[must_use]
    pub fn new(
        day_length_ticks: u32,
        mut keyframes: Vec<SkyFrame>,
        observer: tiamat_core::sky::UniversalPos,
    ) -> Self {
        // Sorted defensively. The server sorts too, but this is data from a
        // peer and an out-of-order list would make the sky walk backwards
        // partway through the day rather than fail in any visible way.
        keyframes.sort_by(|a, b| a.time.total_cmp(&b.time));
        Self {
            day_length_ticks,
            keyframes,
            time: 0.0,
            observer,
        }
    }

    /// Where in the universe this sky is seen from.
    #[must_use]
    pub const fn observer(&self) -> tiamat_core::sky::UniversalPos {
        self.observer
    }

    /// How far the stars have wheeled at this moment, as `(cos, sin)` of the
    /// day's turn — the same turn the server holds a gaze against, so the
    /// star drawn is the star named. See [`tiamat_core::sky::turn`].
    #[must_use]
    pub fn turn(&self) -> (f32, f32) {
        tiamat_core::sky::turn(self.time)
    }

    /// Whether a mod gave this world a day.
    #[must_use]
    pub fn has_day(&self) -> bool {
        self.day_length_ticks > 0 && !self.keyframes.is_empty()
    }

    /// Moves the clock to where the server says it is.
    pub fn set_time(&mut self, time: f32) {
        // `rem_euclid` rather than a clamp: a peer sending 1.5 means the middle
        // of the next day, and clamping would stick the sky at midnight until
        // the next update rather than showing noon.
        self.time = if time.is_finite() {
            time.rem_euclid(1.0)
        } else {
            0.0
        };
    }

    /// Advances the clock by a frame's worth of time.
    ///
    /// **Between the server's updates, not instead of them.** The server sends
    /// the time once a second and this fills the gap so the sky moves smoothly;
    /// every update snaps it back to the truth. A client that only advanced
    /// locally would drift, which is why `set_time` overwrites rather than
    /// blends.
    pub fn advance(&mut self, seconds: f32) {
        if !self.has_day() {
            return;
        }
        let ticks_per_second = 1.0 / tiamat_core::tick::TICK_DURATION.as_secs_f32();
        let day_seconds = self.day_length_ticks as f32 / ticks_per_second;
        self.time = (self.time + seconds / day_seconds).rem_euclid(1.0);
    }

    /// Where the day stands, `0.0..1.0`.
    #[must_use]
    pub const fn time(&self) -> f32 {
        self.time
    }

    /// The sky at this moment.
    ///
    /// Interpolates between the two keyframes the clock sits between, wrapping
    /// from the last back to the first — a day is a circle, and a sky that cut
    /// hard at midnight would flicker once per day.
    #[must_use]
    pub fn moment(&self) -> Moment {
        let Some(first) = self.keyframes.first() else {
            // No sky: full daylight, unchanging. The same values Task 08 drew
            // with, so a world with no sky mod looks exactly as it did.
            return Moment {
                sky: crate::render::sky_colour(),
                sun: [1.0, 1.0, 1.0],
                intensity: 1.0,
                // A world with no day has the sun somewhere sensible rather
                // than nowhere: straight down would make every shadow a
                // vertical smear and every vertical face unlit.
                sun_direction: NOON,
                stars: 0.0,
                // Ungraded, and it matters that this is exact rather than
                // near-identity: mode 3 skips the LUT entirely for this value,
                // which is what keeps a world with no sky mod pixel-for-pixel
                // what it was before grading existed.
                grade: SkyGrade::NONE,
            };
        };
        let last = self.keyframes.last().unwrap_or(first);

        // Before the first keyframe or after the last: between the last and the
        // first, across midnight.
        let (before, after, span) = if self.time < first.time {
            (last, first, first.time + (1.0 - last.time))
        } else {
            match self
                .keyframes
                .windows(2)
                .find(|pair| self.time >= pair[0].time && self.time < pair[1].time)
            {
                Some(pair) => (&pair[0], &pair[1], pair[1].time - pair[0].time),
                None => (last, first, first.time + (1.0 - last.time)),
            }
        };

        // How far between the two, guarding the case where they coincide: two
        // keyframes at the same time are a mod's mistake rather than a crash.
        let travelled = if self.time >= before.time {
            self.time - before.time
        } else {
            self.time + (1.0 - before.time)
        };
        let blend = if span > f32::EPSILON {
            (travelled / span).clamp(0.0, 1.0)
        } else {
            0.0
        };

        Moment {
            sky: mix(before.sky, after.sky, blend),
            sun: mix(before.sun, after.sun, blend),
            intensity: before.intensity + (after.intensity - before.intensity) * blend,
            sun_direction: self.sun_direction(),
            // Sanitised like the grade: a peer's NaN would be a sky of stars
            // at noon, or none at midnight, with nothing to say why.
            stars: {
                let stars = before.stars + (after.stars - before.stars) * blend;
                if stars.is_finite() {
                    stars.clamp(0.0, 1.0)
                } else {
                    0.0
                }
            },
            grade: mix_grade(&before.grade, &after.grade, blend),
        }
    }

    /// Which way the sunlight travels at this moment, normalised.
    ///
    /// The sun rises in the east at 0.25, stands highest at noon, and sets in
    /// the west at 0.75 — the convention the keyframes in `game/core_sky` are
    /// written against. It never passes exactly overhead: a sun straight up
    /// gives every vertical face the same light and every shadow zero length,
    /// which reads as a mistake even though it is geometry. [`TILT`] is what
    /// keeps a shadow on the ground at noon.
    ///
    /// **The arc is the client's, not the mod's.** A mod says how long a day is
    /// and what colour it goes; where the sun sits is geometry the renderer
    /// needs whether or not anyone described it. A mod-chosen axis is a
    /// reasonable thing to add later and nothing here forecloses it.
    ///
    /// `sin` and `cos` are fine here and would not be in the simulation:
    /// charter rule 4 is explicit that rendering is outside the deterministic
    /// float subset. Nothing in this function reaches the tick or the hash gate.
    #[expect(
        clippy::disallowed_methods,
        reason = "charter rule 4 exempts rendering from the deterministic float subset; where the                   sun is drawn never reaches the tick or the hash gate"
    )]
    #[must_use]
    pub fn sun_direction(&self) -> [f32; 3] {
        if !self.has_day() {
            return NOON;
        }
        // Midnight is 0, so the sun is below the world; noon is 0.5 and it is
        // above. The angle runs a full turn over the day.
        let angle = (self.time - 0.25) * std::f32::consts::TAU;
        let height = angle.sin();
        let east = angle.cos();
        normalise([east, -height, TILT])
    }
}

/// How far the sun leans out of the east-west plane, as a fraction.
///
/// Without it the sun passes exactly overhead at noon, every shadow collapses
/// to nothing, and the two vertical faces along its axis are lit identically.
/// A quarter is enough to keep shadows on the ground all day without making
/// noon look like afternoon.
const TILT: f32 = 0.25;

/// Where the sun sits in a world with no day, and at noon.
///
/// Down and a little to one side, normalised.
const NOON: [f32; 3] = [0.0, -0.970_142_5, 0.242_535_62];

/// A unit vector in the same direction, or [`NOON`] if there is no direction to
/// speak of. Zero-length input is a caller's bug rather than a crash.
fn normalise(v: [f32; 3]) -> [f32; 3] {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if length < f32::EPSILON {
        return NOON;
    }
    [v[0] / length, v[1] / length, v[2] / length]
}

/// Linear blend between two grades, sanitised.
///
/// **The sanitising is not defensive tidiness.** These numbers come from a
/// server the client has no reason to trust (charter rule 14). A `NaN` `gamma`
/// or a `tint` of `1e30` reaches a `powf` in the LUT bake and comes out as a
/// frame of one flat colour, with nothing on screen to say why — so out-of-range
/// values are clamped to the same bounds `register_sky` enforces, and anything
/// non-finite falls back to the identity for that field.
fn mix_grade(from: &SkyGrade, to: &SkyGrade, blend: f32) -> SkyGrade {
    let scalar = |from: f32, to: f32, identity: f32, low: f32| -> f32 {
        if !from.is_finite() || !to.is_finite() {
            return identity;
        }
        (from + (to - from) * blend).clamp(low, GRADE_MAX)
    };
    let colour = |from: [f32; 3], to: [f32; 3], identity: [f32; 3], low: f32| -> [f32; 3] {
        let mixed = mix(from, to, blend);
        let mut out = identity;
        for channel in 0..3 {
            if mixed[channel].is_finite() {
                out[channel] = mixed[channel].clamp(low, GRADE_MAX);
            }
        }
        out
    };
    SkyGrade {
        exposure: scalar(from.exposure, to.exposure, 1.0, 0.0),
        tint: colour(from.tint, to.tint, SkyGrade::NONE.tint, 0.0),
        offset: colour(from.offset, to.offset, SkyGrade::NONE.offset, -1.0),
        contrast: scalar(from.contrast, to.contrast, 1.0, 0.0),
        saturation: scalar(from.saturation, to.saturation, 1.0, 0.0),
        // Never zero: the bake raises each channel to this power, and a zero
        // exponent maps every colour in the frame to white.
        gamma: scalar(from.gamma, to.gamma, 1.0, GRADE_MIN_GAMMA),
    }
}

/// The bounds a graded value is held to, matching `register_sky`'s.
///
/// Restated here rather than shared, because these two checks answer different
/// questions: that one tells a mod author they made a mistake, and this one
/// stops a hostile or buggy server from painting the screen one colour.
const GRADE_MAX: f32 = 4.0;
const GRADE_MIN_GAMMA: f32 = 0.1;

/// Linear blend between two colours.
fn mix(from: [f32; 3], to: [f32; 3], blend: f32) -> [f32; 3] {
    [
        from[0] + (to[0] - from[0]) * blend,
        from[1] + (to[1] - from[1]) * blend,
        from[2] + (to[2] - from[2]) * blend,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal two-keyframe day, for the tests that only need a sky to
    /// exist rather than to be any particular colour.
    fn frames() -> Vec<SkyFrame> {
        vec![frame(0.0, 0.1, 0.1), frame(0.5, 1.0, 1.0)]
    }

    fn frame(time: f32, value: f32, intensity: f32) -> SkyFrame {
        SkyFrame {
            time,
            sky: [value; 3],
            sun: [value; 3],
            intensity,
            grade: SkyGrade::NONE,
            stars: 0.0,
        }
    }

    /// Where the tests' skies are seen from: nowhere in particular.
    const HERE: tiamat_core::sky::UniversalPos = tiamat_core::sky::UniversalPos::CENTRE;

    /// A keyframe that grades, for the tests about grading.
    fn graded(time: f32, saturation: f32) -> SkyFrame {
        SkyFrame {
            grade: SkyGrade {
                saturation,
                ..SkyGrade::NONE
            },
            ..frame(time, 0.5, 0.5)
        }
    }

    #[test]
    fn a_world_with_no_sky_mod_is_permanently_daylight() {
        // Charter rule 1: no sky registered is a world without a day, not a
        // broken one — and it must look exactly like Task 08's scenes, whose
        // screenshot hashes still have to hold.
        let sky = Sky::none();
        assert!(!sky.has_day());
        let moment = sky.moment();
        assert!((moment.intensity - 1.0).abs() < 1e-6);
        assert!(
            moment
                .sun
                .iter()
                .all(|channel| (channel - 1.0).abs() < 1e-6),
            "a world with no sky should be lit by a white sun: {:?}",
            moment.sun
        );
    }

    #[test]
    fn advancing_a_world_with_no_day_does_nothing() {
        let mut sky = Sky::none();
        sky.advance(1_000.0);
        assert!((sky.time() - 0.0).abs() < 1e-6);
    }

    #[test]
    fn the_clock_lands_between_the_keyframes_it_sits_between() {
        let mut sky = Sky::new(100, vec![frame(0.0, 0.0, 0.0), frame(1.0, 1.0, 1.0)], HERE);
        sky.set_time(0.25);
        let moment = sky.moment();
        assert!(
            (moment.intensity - 0.25).abs() < 1e-5,
            "a quarter of the way should be a quarter lit, got {}",
            moment.intensity
        );
        assert!((moment.sky[0] - 0.25).abs() < 1e-5);
    }

    #[test]
    fn the_day_wraps_at_midnight_rather_than_cutting() {
        // **The bug a naive lookup produces**: a sky that snaps from the last
        // keyframe to the first once a day. Between the last keyframe and 1.0,
        // the sky is on its way back to the first one.
        let mut sky = Sky::new(100, vec![frame(0.0, 0.0, 0.0), frame(0.5, 1.0, 1.0)], HERE);
        sky.set_time(0.75);
        let midway = sky.moment();
        assert!(
            midway.intensity > 0.0 && midway.intensity < 1.0,
            "three quarters through should be between the two, got {}",
            midway.intensity
        );

        // And just before midnight it is nearly back to the first keyframe.
        sky.set_time(0.99);
        let nearly = sky.moment();
        assert!(
            nearly.intensity < midway.intensity,
            "the sky should still be darkening towards midnight: {} then {}",
            midway.intensity,
            nearly.intensity
        );
    }

    #[test]
    fn keyframes_out_of_order_are_sorted_rather_than_trusted() {
        // Data from a peer. An unsorted list would make the sky walk backwards
        // partway through the day rather than fail in any way anyone could see.
        let mut sky = Sky::new(100, vec![frame(1.0, 1.0, 1.0), frame(0.0, 0.0, 0.0)], HERE);
        sky.set_time(0.25);
        assert!((sky.moment().intensity - 0.25).abs() < 1e-5);
    }

    #[test]
    fn a_time_outside_the_day_wraps_into_it() {
        // A peer sending 1.5 means the middle of the next day. Clamping would
        // hold the sky at midnight until the next update.
        let mut sky = Sky::new(100, vec![frame(0.0, 0.0, 0.0), frame(1.0, 1.0, 1.0)], HERE);
        sky.set_time(1.5);
        assert!((sky.time() - 0.5).abs() < 1e-6);
        // And a non-finite value is a peer sending nonsense, which must not
        // become a NaN colour.
        sky.set_time(f32::NAN);
        assert!(sky.time().is_finite());
    }

    #[test]
    fn two_keyframes_at_the_same_moment_do_not_divide_by_zero() {
        // A mod's mistake, not a crash.
        let mut sky = Sky::new(100, vec![frame(0.5, 0.0, 0.0), frame(0.5, 1.0, 1.0)], HERE);
        sky.set_time(0.5);
        assert!(sky.moment().intensity.is_finite());
    }

    #[test]
    fn advancing_covers_the_whole_day_in_the_length_the_mod_set() {
        // 100 ticks at 20 Hz is five seconds, so five seconds of advancing
        // should return the clock to where it started.
        let mut sky = Sky::new(100, vec![frame(0.0, 0.0, 0.0), frame(1.0, 1.0, 1.0)], HERE);
        sky.advance(2.5);
        assert!(
            (sky.time() - 0.5).abs() < 1e-4,
            "half a day should be halfway, got {}",
            sky.time()
        );
        sky.advance(2.5);
        assert!(
            sky.time() < 1e-4 || sky.time() > 1.0 - 1e-4,
            "the day did not wrap"
        );
    }

    #[test]
    fn the_sun_rises_in_the_east_and_sets_in_the_west() {
        // The convention `game/core_sky`'s keyframes are written against, and
        // the one shadow directions depend on. Stated as a test because it is
        // otherwise only recorded in the sign of a `cos`.
        let mut sky = Sky::new(24_000, frames(), HERE);

        sky.set_time(0.25);
        let dawn = sky.sun_direction();
        sky.set_time(0.75);
        let dusk = sky.sun_direction();

        assert!(
            dawn[0] > 0.5,
            "at dawn the light should travel eastward, got {dawn:?}"
        );
        assert!(
            dusk[0] < -0.5,
            "at dusk it should travel westward, got {dusk:?}"
        );

        sky.set_time(0.5);
        let noon = sky.sun_direction();
        assert!(
            noon[1] < -0.9,
            "at noon the light should come from almost overhead, got {noon:?}"
        );
        assert!(
            noon[1] > -1.0,
            "but never exactly overhead, or every shadow has no length: {noon:?}"
        );

        sky.set_time(0.0);
        assert!(
            sky.sun_direction()[1] > 0.9,
            "at midnight the sun is under the world, so its light travels upward"
        );
    }

    #[test]
    fn the_sun_direction_is_always_a_unit_vector() {
        // Shadow maths assumes it. A direction that drifted off unit length
        // would stretch the cascades by however much it drifted.
        let mut sky = Sky::new(24_000, frames(), HERE);
        for step in 0..64 {
            #[allow(
                clippy::cast_precision_loss,
                reason = "a test index, not a measurement"
            )]
            let time = step as f32 / 64.0;
            sky.set_time(time);
            let direction = sky.sun_direction();
            let length = (direction[0] * direction[0]
                + direction[1] * direction[1]
                + direction[2] * direction[2])
                .sqrt();
            assert!(
                (length - 1.0).abs() < 1e-5,
                "at {time} the direction {direction:?} has length {length}"
            );
        }
    }

    #[test]
    fn the_grade_interpolates_between_keyframes_like_the_colours_do() {
        // A grade that jumped at each keyframe would be a visible step in the
        // middle of a fade the rest of the sky is doing smoothly.
        let mut sky = Sky::new(100, vec![graded(0.0, 0.0), graded(1.0, 1.0)], HERE);
        sky.set_time(0.5);
        let saturation = sky.moment().grade.saturation;
        assert!(
            (saturation - 0.5).abs() < 1e-5,
            "halfway between 0 and 1 should be 0.5, got {saturation}"
        );
    }

    #[test]
    fn a_sky_that_grades_nothing_reports_the_exact_identity() {
        // Mode 3 skips the grading table on exactly this comparison, and the
        // skip is what keeps an ungraded world pixel-for-pixel what it was. A
        // grade that arrived as 0.999999 would grade every frame for ever.
        let mut sky = Sky::new(100, frames(), HERE);
        for step in 0..16 {
            sky.set_time(step as f32 / 16.0);
            assert_eq!(
                sky.moment().grade,
                SkyGrade::NONE,
                "an ungraded sky graded at {}",
                sky.time()
            );
        }
    }

    #[test]
    fn a_hostile_grade_cannot_reach_the_bake() {
        // Charter rule 14: this arrives from a server the client has no reason
        // to trust. A NaN gamma reaches a `powf` and comes out as a frame of one
        // flat colour with nothing on screen to say why.
        let poison = SkyFrame {
            grade: SkyGrade {
                exposure: f32::NAN,
                tint: [f32::INFINITY; 3],
                offset: [-9.0; 3],
                contrast: 1e30,
                saturation: f32::NAN,
                gamma: 0.0,
            },
            ..frame(0.0, 0.5, 0.5)
        };
        let mut sky = Sky::new(100, vec![poison, frame(1.0, 0.5, 0.5)], HERE);
        sky.set_time(0.0);
        let grade = sky.moment().grade;

        assert!(grade.exposure.is_finite() && grade.exposure > 0.0);
        assert!(grade.saturation.is_finite());
        assert!(grade.contrast.is_finite() && grade.contrast <= GRADE_MAX);
        assert!(
            grade.gamma >= GRADE_MIN_GAMMA,
            "a gamma of {} maps the whole frame to white",
            grade.gamma
        );
        for channel in 0..3 {
            assert!(grade.tint[channel].is_finite() && grade.tint[channel] <= GRADE_MAX);
            assert!(grade.offset[channel] >= -1.0, "{:?}", grade.offset);
        }
    }

    #[test]
    fn a_world_with_no_day_still_has_a_sun_to_cast_shadows_from() {
        let direction = Sky::none().sun_direction();
        assert!(
            direction[1] < -0.5,
            "the light should still come downward: {direction:?}"
        );
        // The moment and the direct call must agree: two ways to ask the same
        // question, and a shadow map reading one while the shader reads the
        // other would light the world from two different suns.
        let from_moment = Sky::none().moment().sun_direction;
        for axis in 0..3 {
            assert!(
                (direction[axis] - from_moment[axis]).abs() < f32::EPSILON,
                "{direction:?} against {from_moment:?}"
            );
        }
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "the values asserted are set, not computed"
    )]
    fn a_modifier_eases_from_where_it_is_and_arrives_exactly() {
        // **Weather ask W1.** Set at once, it is there; set over a second, it
        // is halfway at half a second; called off, it goes back over the
        // same time from wherever it had got to; and at its identity it
        // leaves a moment bit-identical.
        let storm = SkyModifier {
            intensity: 0.5,
            sky: [0.5, 0.5, 0.5],
            sky_mix: 1.0,
            fog_distance: 0.5,
            saturation: 0.5,
            ease_ticks: 20,
        };
        let mut eased = Eased::none();
        eased.set(Some(SkyModifier {
            ease_ticks: 0,
            ..storm
        }));
        assert_eq!(eased.current().intensity, 0.5, "no ease is at once");

        let mut eased = Eased::none();
        eased.set(Some(storm));
        assert_eq!(eased.current().intensity, 1.0, "it starts where it was");
        eased.advance(0.5);
        let half = eased.current();
        assert!((half.intensity - 0.75).abs() < 1e-6, "{half:?}");
        assert!((half.fog_distance - 0.75).abs() < 1e-6);
        eased.set(None);
        assert!(
            (eased.current().intensity - 0.75).abs() < 1e-6,
            "called off from where it was"
        );
        eased.advance(1.0);
        assert_eq!(
            eased.current(),
            SkyModifier::NONE,
            "and back to the plain sky, exactly"
        );

        let plain = Sky::none().moment();
        assert_eq!(
            modified(plain, &SkyModifier::NONE),
            plain,
            "the identity is bit-identical"
        );
        let dim = modified(plain, &storm);
        assert_eq!(dim.intensity, 0.5);
        assert_eq!(dim.sky, [0.5, 0.5, 0.5]);
        assert_eq!(dim.grade.saturation, 0.5);
    }

    #[test]
    fn a_flash_rises_over_its_attack_falls_over_its_decay_and_is_gone() {
        // **Weather ask W3.** One tick up, four down: at half a tick it is
        // half way up, at the peak it is all there, two ticks into the decay
        // it is half gone, and after five ticks nothing is left and the
        // moment is the moment it was.
        let tick = tiamat_core::tick::TICK_DURATION.as_secs_f32();
        let mut flashes = Flashes::default();
        flashes.strike(Flash {
            intensity: 2.0,
            colour: [0.5, 0.5, 1.0],
            attack_ticks: 1,
            decay_ticks: 4,
        });
        assert!(flashes.light().0.abs() < 1e-6, "nothing yet");
        flashes.advance(tick * 0.5);
        assert!(
            (flashes.light().0 - 1.0).abs() < 1e-5,
            "{:?}",
            flashes.light()
        );
        flashes.advance(tick * 0.5);
        assert!((flashes.light().0 - 2.0).abs() < 1e-5, "the peak");
        let plain = Sky::none().moment();
        let lit = flashed(plain, &flashes);
        assert!(lit.intensity > plain.intensity);
        assert!(
            lit.sun[2] > lit.sun[0],
            "the sun leans to the flash's colour"
        );
        flashes.advance(tick * 2.0);
        assert!((flashes.light().0 - 1.0).abs() < 1e-5, "half decayed");
        flashes.advance(tick * 3.0);
        assert!(flashes.is_empty(), "died away and forgotten");
        assert_eq!(
            flashed(plain, &flashes),
            plain,
            "and the moment is exactly what it was"
        );
    }

    #[test]
    fn a_deck_eases_from_clear_to_storm_over_its_ticks_and_arrives_exactly() {
        // Weather ask W18's second half: `set_clouds` from clear to storm
        // with `ease_ticks = 600` is not overcast on the next frame and is by
        // the thirtieth second.
        let storm = Clouds {
            cover: 1.0,
            darkness: 1.0,
            base: Some(300.0),
            ease_ticks: 600,
            stratocumulus: 0.5,
            altocumulus: 0.0,
            cumulonimbus: 1.0,
        };
        let mut deck = EasedDeck::default();
        assert_eq!(deck.current(), None, "no state at all draws no deck");
        deck.set(Some(storm));
        deck.advance(1.0 / 60.0);
        let first = deck.current().expect("a state on the way");
        assert!(first.cover < 0.01, "overcast on the next frame: {first:?}");
        assert!(first.darkness < 0.01);
        assert_eq!(
            first.base,
            Some(300.0),
            "a floor appearing steps rather than eases from nothing"
        );
        deck.advance(15.0);
        let half = deck.current().expect("a state on the way");
        assert!(
            (half.cover - 0.5).abs() < 0.01,
            "half way after fifteen seconds: {half:?}"
        );
        assert!((half.cumulonimbus - 0.5).abs() < 0.01);
        deck.advance(15.0);
        assert_eq!(deck.current(), Some(storm), "arrived exactly, not nearly");
        assert_eq!(deck.target(), Some(storm));

        // Clearing takes as long as the storm took to arrive.
        deck.set(None);
        deck.advance(15.0);
        let fading = deck.current().expect("still fading");
        assert!((fading.cover - 0.5).abs() < 0.01, "{fading:?}");
        deck.advance(15.0);
        assert_eq!(deck.current(), None);
    }

    #[test]
    fn a_newcomers_first_sky_with_no_ticks_is_at_once() {
        let mut deck = EasedDeck::default();
        deck.set(Some(Clouds {
            cover: 0.8,
            ..CLEAR
        }));
        let now = deck.current().expect("a state");
        assert!((now.cover - 0.8).abs() < f32::EPSILON);
    }

    fn grid(darkness: u8, size: u8) -> std::sync::Arc<CloudMap> {
        let count = usize::from(size) * usize::from(size);
        std::sync::Arc::new(CloudMap {
            origin: [0.0, 0.0],
            cell: 256.0,
            size,
            cover: vec![255; count],
            darkness: vec![darkness; count],
            stratocumulus: Vec::new(),
            altocumulus: Vec::new(),
            cumulonimbus: Vec::new(),
        })
    }

    #[test]
    fn a_map_re_sent_with_a_cell_darker_does_not_step_that_cell_in_one_frame() {
        let mut map = EasedMap::default();
        map.set(Some(grid(0, 4)), 0, &CLEAR);
        assert_eq!(map.current(&CLEAR).expect("a map").darkness[5], 0);
        let mut darker = (*grid(0, 4)).clone();
        darker.darkness[5] = 200;
        map.set(Some(std::sync::Arc::new(darker.clone())), 600, &CLEAR);
        map.advance(1.0 / 60.0);
        let soon = map.current(&CLEAR).expect("a map on the way");
        assert!(
            soon.darkness[5] < 10,
            "the cell stepped: {}",
            soon.darkness[5]
        );
        assert_eq!(soon.darkness[4], 0, "a cell that did not change moved");
        map.advance(15.0);
        let half = map.current(&CLEAR).expect("a map on the way");
        assert!(
            (95..=105).contains(&half.darkness[5]),
            "half way: {}",
            half.darkness[5]
        );
        map.advance(15.0);
        assert_eq!(*map.current(&CLEAR).expect("arrived"), darker);
        assert_eq!(map.target().map(|map| map.darkness[5]), Some(200));
    }

    #[test]
    fn a_map_fades_up_from_the_plain_state_and_a_new_grid_steps() {
        // Arriving where there was none: the cells start at the plain
        // state's shares — cover 0.4 here — rather than at nothing.
        let plain = Clouds {
            cover: 0.4,
            ..CLEAR
        };
        let mut map = EasedMap::default();
        map.set(Some(grid(255, 4)), 600, &plain);
        map.advance(1.0 / 60.0);
        let soon = map.current(&plain).expect("a map on the way");
        assert!(
            (100..=104).contains(&soon.cover[0]),
            "from the plain cover: {}",
            soon.cover[0]
        );
        assert!(soon.darkness[0] < 10);
        // A different grid does not blend: its cells are not the old ones.
        map.set(Some(grid(50, 8)), 600, &plain);
        map.advance(1.0 / 60.0);
        let stepped = map.current(&plain).expect("a map");
        assert_eq!(stepped.size, 8);
        assert_eq!(
            stepped.darkness[0], 50,
            "a resized grid should step to the new map"
        );
    }
}
