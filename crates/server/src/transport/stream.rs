// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Per-connection chunk streaming.
//!
//! Tracks what one player has been sent, works out what they still need, and
//! paces the sending so a joining player does not monopolise the simulation.
//!
//! # The interest centre is server-side state
//!
//! There is no protocol message carrying a player's position, and there should
//! not be one: a client that told the server where it was could tell it
//! anything, and every anti-cheat problem in a voxel game starts there. The
//! centre is authoritative server state, initialised to spawn, and Task 09's
//! physics moves it. Until then a player's interest set is the spawn
//! neighbourhood — which is a real limitation, not a placeholder that will be
//! swapped out: the streaming machinery below is complete, and physics only
//! needs to write [`Streamer::recentre`].
//!
//! # Why a sent-set rather than a diff
//!
//! Recomputing "everything within range, minus everything already sent" every
//! pass is O(interest), around 1800 set lookups. Computing a diff against the
//! previous centre would be cheaper, but it goes wrong quietly: a chunk whose
//! send failed, or that was dropped because the queue was full, is absent from
//! both sets and is never retried. The sent-set is self-correcting — anything
//! missing gets picked up on the next pass, whatever went wrong.

use std::collections::{BTreeMap, BTreeSet};

use tiamat_core::ChunkPos;
use tiamat_core::interest::{self, ViewDistance};
use tiamat_core::lod::{Level, Rings, horizon_for};

/// What one connection has been sent, and what it still needs.
pub struct Streamer {
    /// The domain every position in here belongs to.
    ///
    /// **Interest is domain-scoped**, which is not a rule enforced on top of
    /// this but what the sets already mean: a chunk at `(0, 0, 0)` in one
    /// domain and a chunk at `(0, 0, 0)` in another are different chunks, and
    /// a set of positions can only be about one of them.
    domain: String,
    centre: ChunkPos,
    view: ViewDistance,
    /// Chunks the client has, or has been sent.
    ///
    /// A `BTreeSet` rather than a `HashSet`: the iteration order matters for
    /// reproducible unload ordering in tests, and at ~1800 entries the lookup
    /// difference is not measurable next to encoding a chunk.
    sent: BTreeSet<ChunkPos>,
    /// Chunks requested from the simulation but not yet answered.
    ///
    /// A **set of positions**, not a count. A count is not enough: `next_needed`
    /// filters against what has been *delivered*, so a chunk still in flight
    /// looks un-requested and gets asked for a second time. On a fast machine
    /// the reply usually lands before the next pass and it never shows; on a
    /// slower one the client receives the same chunk twice. CI on macOS caught
    /// exactly that.
    in_flight: BTreeSet<ChunkPos>,
    /// How far the horizon reaches, past the detail radius.
    ///
    /// Chunks between `view` and this are sent as summaries. Kept separate
    /// from `view` because they are answers to different questions: `view` is
    /// what the client asked for and the server granted, and this is how much
    /// further the engine is willing to draw the shape of the land for free.
    horizon: ViewDistance,
    /// Which level each distance band takes, and the hysteresis on the edges.
    rings: Rings,
    /// Summaries the client holds, and the level each was sent at.
    ///
    /// A position is in this OR in `sent`, never both: it is one chunk, and a
    /// client holding a coarse copy and a fine one would draw both, with the
    /// coarse one poking through.
    summaries: BTreeMap<ChunkPos, u8>,
    /// Every position in the horizon, nearest first.
    ///
    /// **Cached, because it is 38,000 positions at the default view and
    /// computing it means sorting all of them.** The detail radius can afford
    /// that per pass; the horizon cannot, and it only changes when the player
    /// crosses a chunk boundary or the view distance moves.
    horizon_order: Vec<ChunkPos>,
    /// How far through `horizon_order` the last pass looked.
    ///
    /// A rotating cursor rather than a scan from the start: the pass is looking
    /// for one position out of tens of thousands, nearly all of which are
    /// already held, and starting over each time would spend the whole scan
    /// re-testing the same held entries. Wraps, so everything gets a turn.
    horizon_cursor: usize,
    /// How far out the detail has got: the streaming distance of the nearest
    /// chunk still needed, or `None` when nothing is.
    ///
    /// **The horizon waits behind this.** It had its own allowance and ran
    /// on it from the first pass, so its nearest band — a ring of summaries
    /// just past the detail radius — arrived while the detail was still
    /// filling in from the player's feet outward. Reported from the window
    /// as a ring of chunks far out loading with the spawn chunk, with a gap
    /// between. A summary is now asked for only when it is no further, in
    /// [`interest::stream_distance`]'s order, than the nearest chunk the
    /// detail has yet to send, so the two fill together and the horizon
    /// never shows past a hole. See [`Streamer::next_summaries`].
    frontier: Option<i64>,
    /// Chunks the client holds that are one opaque, unlit material through and
    /// through: a wall nothing behind can be seen past. See [`Self::sealed`].
    sealed: BTreeSet<ChunkPos>,
    /// Positions found to be in shadow — behind sealed rock with no open way in
    /// — so far as anything known could change it. A memo, not a fact: an open
    /// chunk arriving or a sealed one being dug into drops the entries it could
    /// have lifted ([`Self::lift_shadows`]); a move or a resize drops them all;
    /// a sealed chunk arriving drops nothing, because a shadow only ever grows
    /// from that. Kept rather than cleared on every delivery because at the
    /// default view working it all out again costs **1.43 ms** per pass, and a
    /// pass runs every tick for every connection with something in flight;
    /// kept, a whole join's 345 passes cost 152 ms of streamer time, 0.44 ms a
    /// pass, most of it the interest set's own ordering. Measured by
    /// `tests::measure_a_pass_at_the_default_view`, release. See
    /// [`Self::is_shadowed`].
    shadowed: BTreeSet<ChunkPos>,
}

/// How many horizon positions one pass will look at before giving up.
///
/// The scan is cheap per position — two set lookups and some integer
/// arithmetic — but the horizon is tens of thousands of them and this runs on
/// the connection task every pass. A bounded look with a cursor that wraps
/// covers everything within a few passes and costs a fixed amount each time.
const HORIZON_SCAN: usize = 2048;

impl Streamer {
    /// A streamer centred on a player's spawn.
    #[must_use]
    pub fn new(domain: &str, centre: ChunkPos, view: ViewDistance) -> Self {
        let mut streamer = Self {
            domain: domain.to_owned(),
            centre,
            view,
            sent: BTreeSet::new(),
            in_flight: BTreeSet::new(),
            horizon: horizon_for(view),
            rings: Rings::new(u32::from(view.horizontal), Rings::MARGIN),
            summaries: BTreeMap::new(),
            horizon_order: Vec::new(),
            horizon_cursor: 0,
            frontier: None,
            sealed: BTreeSet::new(),
            shadowed: BTreeSet::new(),
        };
        streamer.reorder_horizon();
        streamer
    }

    /// The current interest centre.
    #[must_use]
    pub const fn centre(&self) -> ChunkPos {
        self.centre
    }

    /// Which domain this connection is being streamed from.
    #[must_use]
    pub fn domain(&self) -> &str {
        &self.domain
    }

    /// Moves this connection to another domain, returning everything to drop.
    ///
    /// **Everything, not what left range.** The client holds a set of chunks
    /// belonging to the domain it was in, and none of them mean anything in the
    /// new one — the positions are the same and the contents are not. So the
    /// whole sent-set comes back, the in-flight requests are abandoned, and the
    /// new domain streams in from nothing.
    ///
    /// The caller is expected to tell the client with a single domain-switch
    /// message rather than to send an unload per position: at the default view
    /// distance that is upwards of a thousand messages to say one thing.
    ///
    /// Switching to the domain already being streamed does nothing and returns
    /// nothing, so a caller that checks every tick costs a string compare.
    pub fn switch_to(&mut self, domain: &str, centre: ChunkPos) -> Vec<ChunkPos> {
        if domain == self.domain {
            return Vec::new();
        }
        self.domain = domain.to_owned();
        self.centre = centre;
        self.shadowed.clear();
        self.reorder_horizon();
        // Abandoned rather than awaited. A reply carrying a chunk of the domain
        // this connection has just left would be decoded into the new one at
        // the same coordinates, which is terrain from somewhere else appearing
        // in a place a player is standing.
        self.in_flight.clear();
        // Sealed is a fact about chunks in the OLD domain; the same positions
        // in the new one are different chunks.
        self.sealed.clear();
        let mut dropped: Vec<ChunkPos> = std::mem::take(&mut self.sent).into_iter().collect();
        dropped.extend(std::mem::take(&mut self.summaries).into_keys());
        dropped
    }

    /// How many chunks this client has been sent.
    #[must_use]
    pub fn sent_count(&self) -> usize {
        self.sent.len()
    }

    /// How many requests are outstanding.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.in_flight.len()
    }

    /// Whether every chunk in range has been sent.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.in_flight.is_empty() && self.nothing_needed()
    }

    /// The radius this client is being streamed at.
    #[must_use]
    pub const fn view(&self) -> ViewDistance {
        self.view
    }

    /// Changes the radius, returning chunks that left range.
    ///
    /// The same contract as [`Streamer::recentre`] and for the same reason —
    /// the caller sends a `ChunkUnload` for each returned position, and a
    /// client told to unload has thrown the chunk away, so shrinking and then
    /// growing again streams it back.
    ///
    /// Growing returns nothing: no chunk leaves range when the range gets
    /// bigger, and the new ones arrive through [`Streamer::next_needed`] on the
    /// next pump like any other.
    pub fn resize(&mut self, view: ViewDistance) -> Vec<ChunkPos> {
        if view == self.view {
            return Vec::new();
        }
        self.view = view;
        self.shadowed.clear();
        self.horizon = horizon_for(view);
        self.rings = Rings::new(u32::from(view.horizontal), Rings::MARGIN);
        self.reorder_horizon();
        self.in_flight
            .retain(|pos| interest::contains(self.centre, self.horizon, *pos));
        self.departed()
    }

    /// Moves the interest centre, returning chunks that left range.
    ///
    /// The caller sends a `ChunkUnload` for each. They are forgotten here, so
    /// walking back into an area streams it again — which is correct, because
    /// a client told to unload a chunk has thrown it away.
    pub fn recentre(&mut self, centre: ChunkPos) -> Vec<ChunkPos> {
        if centre == self.centre {
            return Vec::new();
        }
        self.centre = centre;
        self.shadowed.clear();
        self.reorder_horizon();
        // Requests for chunks that just left range are abandoned. Keeping them
        // would deliver a chunk the client was told to unload, and hold budget
        // that the new neighbourhood needs. Against the HORIZON rather than the
        // detail radius: a chunk that left the detail radius has not left the
        // client's world, it has become a summary.
        self.in_flight
            .retain(|pos| interest::contains(centre, self.horizon, *pos));
        self.departed()
    }

    /// Forgets everything past the horizon, and says what left.
    ///
    /// Shared by [`Streamer::recentre`] and [`Streamer::resize`], which differ
    /// only in which of the two numbers moved. A chunk that fell out of the
    /// detail radius but is still inside the horizon is NOT departed — it is
    /// about to be re-sent as a summary, and unloading it first would blink a
    /// hole in the world the size of a chunk.
    fn departed(&mut self) -> Vec<ChunkPos> {
        let horizon = self.horizon;
        let centre = self.centre;
        let mut departed: Vec<ChunkPos> = self
            .sent
            .iter()
            .copied()
            .filter(|pos| !interest::contains(centre, horizon, *pos))
            .collect();
        departed.extend(
            self.summaries
                .keys()
                .copied()
                .filter(|pos| !interest::contains(centre, horizon, *pos)),
        );
        for pos in &departed {
            self.sent.remove(pos);
            self.summaries.remove(pos);
            self.sealed.remove(pos);
        }
        if !departed.is_empty() {
            self.shadowed.clear();
        }
        departed.sort_unstable();
        departed.dedup();
        departed
    }

    /// The next positions to request, nearest first, at most `limit` of them —
    /// everything in range that is not already held or in flight and is not
    /// in shadow behind sealed rock.
    pub fn next_needed(&mut self, limit: usize) -> Vec<ChunkPos> {
        if limit == 0 {
            return Vec::new();
        }
        // **Nearest first, and only what is not in shadow.** The order is the
        // plain streaming distance, as it was before sealing existed: the
        // world fills in from the player outward, continuously, at whatever
        // rate the in-flight budget allows. Sealing only REMOVES positions from
        // that order — see `is_shadowed` — it never makes one wait for another.
        // The first version of this walked a flood from the centre that could
        // only expand through chunks already delivered, and that made the
        // frontier advance one chunk per round trip: a player watched rings
        // arrive one at a time, seven chunks out, and reported exactly that.
        let mut needed = Vec::with_capacity(limit.min(64));
        for pos in interest::chunks_around(self.centre, self.view) {
            if self.sent.contains(&pos) || self.in_flight.contains(&pos) {
                continue;
            }
            if self.shadowed.contains(&pos) {
                continue;
            }
            if self.is_shadowed(pos) {
                self.shadowed.insert(pos);
                continue;
            }
            needed.push(pos);
            if needed.len() == limit {
                break;
            }
        }
        // The nearest of what is still needed is where the detail has got
        // to, and the horizon waits behind it. Nothing needed is a frontier
        // at infinity: the horizon has the sky to itself.
        self.frontier = needed
            .first()
            .map(|pos| interest::stream_distance(self.centre, *pos));
        needed
    }

    /// Whether nothing in range is left to ask for, without touching the memo.
    fn nothing_needed(&self) -> bool {
        !interest::chunks_around(self.centre, self.view)
            .into_iter()
            .any(|pos| {
                !self.sent.contains(&pos)
                    && !self.in_flight.contains(&pos)
                    && !self.shadowed.contains(&pos)
                    && !self.is_shadowed(pos)
            })
    }

    /// Whether `pos` lies behind sealed rock with no open way in, so far as is
    /// known — the one reason a position in range is not asked for.
    ///
    /// # The rule
    ///
    /// Walk from `pos` toward the centre, one chunk at a time along whichever
    /// axis has furthest to go. The first chunk met that the client holds
    /// decides: sealed, and `pos` is in its shadow; open, and there is a clear
    /// line to it. Reaching the centre is a clear line too — the player is
    /// standing in it. Chunks not yet held are walked through, because a chunk
    /// nobody has seen yet is not evidence of anything.
    ///
    /// Before any of that: a face-neighbour the client holds and knows to be
    /// open is a way in, whatever the ray says. That is how a cave under a
    /// solid roof, entered from a shaft at its edge, is streamed chunk by chunk
    /// along its length — and why this is not the rule "skip whatever is under
    /// sealed rock", which would never send it.
    ///
    /// # What it costs, and what it saves
    ///
    /// Requests follow the streaming distance, so a chunk of rock can be asked
    /// for before the chunk between it and the player has arrived and proved
    /// sealed: about one layer of rock, in the columns nearest the player, in
    /// the first passes of a join. After that the shadow is known and the rest
    /// of the rock is never asked for. The flood this replaced wasted nothing
    /// and advanced one chunk per round trip; this wastes a layer and lets the
    /// world arrive by distance.
    fn is_shadowed(&self, pos: ChunkPos) -> bool {
        const FACES: [(i32, i32, i32); 6] = [
            (1, 0, 0),
            (-1, 0, 0),
            (0, 1, 0),
            (0, -1, 0),
            (0, 0, 1),
            (0, 0, -1),
        ];
        let open = |at: ChunkPos| self.sent.contains(&at) && !self.sealed.contains(&at);
        if FACES.iter().any(|(dx, dy, dz)| {
            open(ChunkPos::new(
                pos.x.saturating_add(*dx),
                pos.y.saturating_add(*dy),
                pos.z.saturating_add(*dz),
            ))
        }) {
            return false;
        }
        let mut at = pos;
        loop {
            at = Self::toward(at, self.centre);
            if at == self.centre {
                return false;
            }
            if self.sealed.contains(&at) {
                return true;
            }
            if self.sent.contains(&at) {
                return false;
            }
        }
    }

    /// One chunk from `at` toward `centre`, along the axis with furthest to go.
    ///
    /// Ties go to the vertical first, then x, then z — deterministic, so two
    /// calls agree, and vertical first because rock is stacked in columns and a
    /// ray that climbs early meets the sealed shell soonest.
    fn toward(at: ChunkPos, centre: ChunkPos) -> ChunkPos {
        let (dx, dy, dz) = (
            i64::from(centre.x) - i64::from(at.x),
            i64::from(centre.y) - i64::from(at.y),
            i64::from(centre.z) - i64::from(at.z),
        );
        let (ax, ay, az) = (dx.abs(), dy.abs(), dz.abs());
        if ay >= ax && ay >= az {
            ChunkPos::new(at.x, at.y + dy.signum() as i32, at.z)
        } else if ax >= az {
            ChunkPos::new(at.x + dx.signum() as i32, at.y, at.z)
        } else {
            ChunkPos::new(at.x, at.y, at.z + dz.signum() as i32)
        }
    }

    /// Records that a chunk reached the client and is sealed: one opaque,
    /// unlit material through and through.
    ///
    /// # What this is for
    ///
    /// The detail radius is a cylinder — eight chunks out and twelve up and
    /// down at the default view — and a player standing on the ground asks for
    /// roughly 2,500 chunks below their feet. Nearly all are solid rock, and
    /// none of them can be seen: rock behind rock is not a view of anything.
    /// Every one used to be generated, relit, encoded and sent, which was the
    /// largest share of a join's serve cost spent on terrain nobody would ever
    /// look at, and the cost was paid on the tick.
    ///
    /// A sealed chunk is still SENT — its outer faces are what the player sees
    /// from the open side — but nothing is requested beyond it. What "beyond"
    /// means is [`Self::is_shadowed`].
    ///
    /// Nothing is dropped from the shadow memo: a newly known wall can put
    /// more in shadow, never less, and what it puts there is found on the next
    /// pass. The open case is [`Self::delivered`].
    pub fn delivered_sealed(&mut self, pos: ChunkPos) {
        self.in_flight.remove(&pos);
        self.summaries.remove(&pos);
        self.sent.insert(pos);
        self.sealed.insert(pos);
    }

    /// Records that a sealed chunk has been dug into: it is a wall no longer,
    /// and what lay behind it becomes requestable.
    ///
    /// Any edit will do. A chunk of one material with one cell changed is not
    /// uniform, and the client that made the change is looking straight at the
    /// hole, so whatever is behind it is what they will see next.
    pub fn unseal(&mut self, pos: ChunkPos) {
        if self.sealed.remove(&pos) {
            self.lift_shadows(pos);
        }
    }

    /// Drops from the shadow memo everything that `opened` — a chunk just
    /// known to be open — could have brought out of shadow.
    ///
    /// A position's shadow is decided by its face-neighbours and by the chunks
    /// on its walk toward the centre, and every step of that walk moves one
    /// coordinate toward the centre — so the whole walk lies in the box
    /// between the position and the centre. A chunk outside that box and not
    /// touching the position cannot have changed the answer, and its entry
    /// stays. On flat ground that is nearly all of them: a chunk of sky
    /// arriving above the player is in no rock's box.
    fn lift_shadows(&mut self, opened: ChunkPos) {
        let centre = self.centre;
        let between = |a: i32, b: i32, c: i32| (a.min(c)..=a.max(c)).contains(&b);
        self.shadowed.retain(|pos| {
            let touching =
                (pos.x - opened.x).abs() + (pos.y - opened.y).abs() + (pos.z - opened.z).abs() == 1;
            let on_the_way = between(pos.x, opened.x, centre.x)
                && between(pos.y, opened.y, centre.y)
                && between(pos.z, opened.z, centre.z);
            !(touching || on_the_way)
        });
    }

    /// How many held chunks are sealed.
    #[must_use]
    pub fn sealed_count(&self) -> usize {
        self.sealed.len()
    }

    /// How many more requests this connection may have outstanding.
    #[must_use]
    pub fn budget(&self, in_flight_cap: usize) -> usize {
        in_flight_cap.saturating_sub(self.in_flight.len())
    }

    /// Records that a chunk has been asked for.
    pub fn requested(&mut self, pos: ChunkPos) {
        self.in_flight.insert(pos);
    }

    /// Records that a request came back, whether or not it produced a chunk.
    ///
    /// Called on **every** outcome — delivered, empty, failed. An in-flight
    /// entry only cleared on success would hold its slot forever, until the
    /// connection stopped asking for anything and the player's world stopped
    /// filling in.
    pub fn completed(&mut self, pos: ChunkPos) {
        self.in_flight.remove(&pos);
    }

    /// Records that a chunk reached the client.
    ///
    /// Takes the position out of the summary set for the reason given on
    /// [`Streamer::summarised`]: a client holds one copy of a chunk, and this
    /// one has just replaced a coarse copy with the real thing.
    pub fn delivered(&mut self, pos: ChunkPos) {
        self.in_flight.remove(&pos);
        self.summaries.remove(&pos);
        if self.sent.insert(pos) {
            // A newly held open chunk may be a way into somewhere that was in
            // shadow. A sealed one is [`Self::delivered_sealed`].
            self.lift_shadows(pos);
        }
    }

    /// How far the horizon reaches for this connection.
    #[must_use]
    pub const fn horizon(&self) -> ViewDistance {
        self.horizon
    }

    /// How many summaries this client holds.
    #[must_use]
    pub fn summary_count(&self) -> usize {
        self.summaries.len()
    }

    /// The level a client holds a chunk at, if it holds a summary of it.
    #[must_use]
    pub fn summary_level(&self, pos: ChunkPos) -> Option<u8> {
        self.summaries.get(&pos).copied()
    }

    /// Up to `limit` chunks in the horizon whose summary the client does not
    /// have at the level its distance calls for, nearest first.
    ///
    /// **Hysteresis lives here**, not at the caller: a chunk already held at a
    /// level keeps it until the player is a whole margin past the ring edge, so
    /// somebody pacing across a boundary does not re-send — and the client does
    /// not rebuild — a band of the horizon every step. See
    /// [`tiamat_core::lod::Rings::stable_level`].
    ///
    /// Does not mark anything sent, for the same reason [`Streamer::next_needed`]
    /// does not.
    #[must_use]
    pub fn next_summaries(&mut self, limit: usize) -> Vec<(ChunkPos, u8)> {
        if limit == 0 || self.horizon_order.is_empty() {
            return Vec::new();
        }
        let mut found = Vec::with_capacity(limit.min(HORIZON_SCAN));
        let total = self.horizon_order.len();
        for step in 0..HORIZON_SCAN.min(total) {
            let index = (self.horizon_cursor + step) % total;
            let pos = self.horizon_order[index];
            // **Behind the detail, never ahead of it.** The order is nearest
            // first, so a position past the frontier means every position
            // after it is too: the pass stops here and starts here next
            // time, once the detail has moved on.
            if self
                .frontier
                .is_some_and(|frontier| interest::stream_distance(self.centre, pos) > frontier)
            {
                self.horizon_cursor = index;
                return found;
            }
            if self.in_flight.contains(&pos) {
                continue;
            }
            let held = if self.sent.contains(&pos) {
                // A chunk the client holds in full is at the detail level as
                // far as the hysteresis is concerned: that is what it is
                // drawing, and it is what a level change would replace.
                Some(Level::Chunk)
            } else {
                self.summaries.get(&pos).map(|level| Level::Summary(*level))
            };
            match self.rings.stable_level(held, self.distance(pos)) {
                Level::Chunk => continue,
                Level::Summary(level) if held == Some(Level::Summary(level)) => continue,
                Level::Summary(level) => {
                    found.push((pos, level));
                    if found.len() >= limit {
                        self.horizon_cursor = (index + 1) % total;
                        return found;
                    }
                }
            }
        }
        self.horizon_cursor = (self.horizon_cursor + HORIZON_SCAN.min(total)) % total;
        found
    }

    /// Recomputes the horizon's order around the current centre.
    ///
    /// Called when the centre, the view or the domain moves — never per pass.
    /// The streaming distance the horizon may reach up to: the nearest chunk
    /// the detail still needs, or `None` for no limit. Set by
    /// [`Streamer::next_needed`], which the connection calls every pass.
    #[must_use]
    pub const fn frontier(&self) -> Option<i64> {
        self.frontier
    }

    fn reorder_horizon(&mut self) {
        // **Without the detail radius in it.** Those positions can never be a
        // summary, and they are the NEAREST ones — so a scan that included them
        // spent its whole window rejecting the same 2,601 chunks and came back
        // empty, which is a horizon that starts several passes late for no
        // reason. Every position in this list is a candidate.
        // **And no taller than the surface needs.** The horizon's EXTENT has to
        // contain the detail set — `departed` tests the sent chunks against it
        // — but almost none of that extent is worth summarising. At a view of 8
        // with the vertical of 12 that became the default on 2026-09-05, the
        // annulus is 75,300 positions where it was 27,108, and the difference
        // is sky and buried rock. See `lod::MAX_HORIZON_VERTICAL`.
        let (centre, view) = (self.centre, self.view);
        let layers = i32::from(tiamat_core::lod::MAX_HORIZON_VERTICAL);
        self.horizon_order = interest::chunks_around(centre, self.horizon)
            .into_iter()
            .filter(|pos| !interest::contains(centre, view, *pos))
            .filter(|pos| (pos.y - centre.y).abs() <= layers)
            .collect();
        self.horizon_cursor = 0;
    }

    /// The horizontal distance from the centre, in chunks, rounded up.
    ///
    /// **The same shape as the interest set, which is a CYLINDER.**
    /// [`interest::contains`] admits a chunk when `dx² + dz²` is within the
    /// radius squared and `dy` is within the vertical bound, so the vertical
    /// takes no part in the radius and the horizontal is Euclidean, not
    /// Chebyshev. Rounding up makes `self.distance(pos) <= view.horizontal`
    /// agree with `contains` exactly, which is the property that matters: a
    /// position is either inside the detail radius or gets a summary, never
    /// both and never neither.
    ///
    /// This was the Chebyshev distance until 2026-09-04, justified by an
    /// interest set that was a box. It has been a cylinder for as long as
    /// `chunks_around` has existed, and the disagreement left a band that
    /// [`Streamer::next_needed`] never sent in full — outside the cylinder —
    /// and `next_summaries` never summarised either, because
    /// [`Rings::level_at`] called it detail. Four lobes on the diagonals,
    /// 41% of the horizon at a view distance of 24 and only 2% at 8, which is
    /// why it went unseen until somebody set the view distance high.
    fn distance(&self, pos: ChunkPos) -> u32 {
        // Saturating throughout: this is a general method, and a caller asking
        // about a chunk on the far side of the world should get a very large
        // distance rather than a wrapped one that reads as "nearby".
        let dx = u64::from(pos.x.abs_diff(self.centre.x));
        let dz = u64::from(pos.z.abs_diff(self.centre.z));
        let squared = dx.saturating_mul(dx).saturating_add(dz.saturating_mul(dz));
        let root = squared.isqrt();
        let rounded = if root.saturating_mul(root) == squared {
            root
        } else {
            root.saturating_add(1)
        };
        u32::try_from(rounded).unwrap_or(u32::MAX)
    }

    /// Records that a summary reached the client.
    ///
    /// Takes the position out of the full-chunk set: it is one chunk, and the
    /// client has just replaced what it held with a coarser copy.
    pub fn summarised(&mut self, pos: ChunkPos, level: u8) {
        self.in_flight.remove(&pos);
        self.sent.remove(&pos);
        self.summaries.insert(pos, level);
    }

    /// Forgets a summary, so the next pass sends it again.
    ///
    /// What an edit in a summarised chunk costs. A block delta is no use to a
    /// client holding a summary — it has nowhere to put one cell of 27 — so the
    /// horizon is re-sent instead.
    pub fn resummarise(&mut self, pos: ChunkPos) {
        self.summaries.remove(&pos);
    }

    /// Whether a chunk is one this client holds.
    ///
    /// Used to decide whether a block edit is worth forwarding: an edit in a
    /// chunk the client has never seen is noise.
    #[must_use]
    pub fn holds(&self, pos: ChunkPos) -> bool {
        self.sent.contains(&pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: ChunkPos = ChunkPos::new(0, 0, 0);

    /// Every summary the streamer will ask for, sending each as it goes.
    ///
    /// **A loop, because one call is bounded.** `next_summaries` looks at a
    /// fixed slice of the horizon per call and rotates — the horizon is tens of
    /// thousands of positions and this runs on the connection task every pass,
    /// so a call that scanned all of them would be the cost the cursor exists
    /// to avoid.
    fn drain_horizon(streamer: &mut Streamer) -> Vec<(ChunkPos, u8)> {
        // **Never stops at the first empty batch.** The cursor rotates, so a
        // window with nothing in it says nothing about the rest of the horizon
        // — stopping there is how this helper first missed a chunk that was
        // waiting a few thousand positions further round.
        let mut all = Vec::new();
        for _ in 0..64 {
            let batch = streamer.next_summaries(usize::MAX);
            for (pos, level) in &batch {
                streamer.summarised(*pos, *level);
            }
            all.extend(batch);
        }
        all
    }

    /// Every chunk on the horizon is eventually summarised — nothing falls
    /// down the gap between the detail radius and the rings.
    ///
    /// **Written after the window showed a ring of nothing.** The interest set
    /// is a cylinder and [`Rings::level_at`] measured a box, so a chunk out
    /// past the detail cylinder but still inside the detail box was sent by
    /// neither path: too far for a full chunk, too near for a summary. It read
    /// from inside the game as a gap between the terrain around you and the
    /// terrain on the horizon, in four lobes on the diagonals.
    ///
    /// **At a view distance of 24, not the default.** The hole was 41% of the
    /// horizon at 24 and 2% at 8 — the corners of a box grow on the circle
    /// inside it — so every test written at the default view distance passed
    /// through it. A test for a shape mismatch has to be run at the distance
    /// that makes the shapes differ.
    #[test]
    fn the_horizon_leaves_no_ring_uncovered() {
        let view = ViewDistance::clamped(24, 12);
        let mut streamer = Streamer::new(tiamat_core::domain::OVERWORLD, ORIGIN, view);
        let covered: BTreeSet<ChunkPos> = drain_horizon(&mut streamer)
            .into_iter()
            .map(|(pos, _)| pos)
            .collect();

        // **The annulus, bounded vertically.** Positions more than
        // `lod::MAX_HORIZON_VERTICAL` layers from the player are deliberately
        // not summarised — sky and buried rock, three quarters of the extent at
        // the default vertical. What this test is about is that nothing INSIDE
        // the band is skipped, which is the shape-mismatch bug it caught.
        let layers = i32::from(tiamat_core::lod::MAX_HORIZON_VERTICAL);
        let wanted: Vec<ChunkPos> = interest::chunks_around(ORIGIN, horizon_for(view))
            .into_iter()
            .filter(|pos| !interest::contains(ORIGIN, view, *pos))
            .filter(|pos| (pos.y - ORIGIN.y).abs() <= layers)
            .collect();
        let missing: Vec<ChunkPos> = wanted
            .iter()
            .copied()
            .filter(|pos| !covered.contains(pos))
            .collect();

        assert!(
            missing.is_empty(),
            "{} of {} horizon chunks were never summarised, e.g. {:?}",
            missing.len(),
            wanted.len(),
            &missing[..missing.len().min(5)]
        );
    }

    /// The ring metric and the interest set agree on where the detail ends.
    ///
    /// The property the test above catches by construction, asserted directly
    /// so a failure names the disagreement rather than a count of holes.
    #[test]
    fn a_chunk_is_detail_exactly_when_the_interest_set_holds_it() {
        for horizontal in [2u8, 4, 8, 16, 24] {
            let view = ViewDistance::clamped(horizontal, 12);
            let streamer = Streamer::new(tiamat_core::domain::OVERWORLD, ORIGIN, view);
            let rings = Rings::new(u32::from(view.horizontal), Rings::MARGIN);
            for pos in interest::chunks_around(ORIGIN, horizon_for(view)) {
                assert_eq!(
                    rings.level_at(streamer.distance(pos)) == Level::Chunk,
                    interest::contains(ORIGIN, view, pos),
                    "at view {horizontal}, {pos:?} is claimed as detail by one and not the other"
                );
            }
        }
    }

    fn streamer() -> Streamer {
        Streamer::new(
            tiamat_core::domain::OVERWORLD,
            ORIGIN,
            ViewDistance::MINIMUM,
        )
    }

    /// Three chunks out, five up and down: room below the centre for rock,
    /// caves and what lies under them.
    const TALL: ViewDistance = ViewDistance {
        horizontal: 3,
        vertical: 5,
    };

    /// A streamer with room below the centre for rock, caves and what lies
    /// under them: three chunks out, five up and down.
    fn tall_streamer() -> Streamer {
        Streamer::new(tiamat_core::domain::OVERWORLD, ORIGIN, TALL)
    }

    /// Delivers everything needed, round after round, until nothing is —
    /// what a connection does over successive passes. In an open world that
    /// is one round; where sealing plays, a delivery can open the way to more.
    fn deliver_all(streamer: &mut Streamer) {
        loop {
            let needed = streamer.next_needed(usize::MAX);
            if needed.is_empty() {
                return;
            }
            for pos in needed {
                streamer.delivered(pos);
            }
        }
    }

    /// Delivers everything currently needed, sealing the positions `wall` says
    /// are rock, until nothing more is asked for. Returns every position sent.
    fn stream_all(streamer: &mut Streamer, wall: impl Fn(ChunkPos) -> bool) -> BTreeSet<ChunkPos> {
        // **Eight at a time**, the in-flight cap one player gets. Delivering
        // the whole cylinder in one round would ask for every chunk before any
        // had arrived and proved sealed, which no real connection does; what
        // sealing saves depends on the nearest rock arriving before the rock
        // behind it is asked for, and eight a pass is what makes that so.
        let mut all = BTreeSet::new();
        loop {
            let needed = streamer.next_needed(8);
            if needed.is_empty() {
                return all;
            }
            for pos in needed {
                streamer.requested(pos);
                if wall(pos) {
                    streamer.delivered_sealed(pos);
                } else {
                    streamer.delivered(pos);
                }
                all.insert(pos);
            }
        }
    }

    #[test]
    fn nothing_behind_a_sealed_chunk_is_ever_requested() {
        // **The reason sealing exists.** Solid rock from one chunk below the
        // centre downward: the first layer of rock is sent — its top faces are
        // the ground the player stands on — and it casts a shadow over the
        // rest, which is never asked for once it has arrived.
        // Without sealing, every position in the cylinder is sent, and at the
        // default view that is thousands of chunks of rock nobody can see.
        let mut streamer = tall_streamer();
        let all = stream_all(&mut streamer, |pos| pos.y < ORIGIN.y);
        let deepest = all
            .iter()
            .map(|pos| pos.y)
            .min()
            .expect("something was sent");
        // The first layer of rock is the shell and is sent. One layer under it
        // MAY be asked for in the columns nearest the player, before the shell
        // above it has arrived and proved sealed — the price of asking by
        // distance rather than waiting on round trips. Nothing deeper, ever.
        assert!(
            deepest >= ORIGIN.y - 2,
            "chunks were sent more than one layer under the first layer of rock: {all:?}"
        );
        let wasted = all.iter().filter(|pos| pos.y < ORIGIN.y - 1).count();
        assert!(
            wasted <= 6,
            "{wasted} chunks of rock under the shell were asked for; the shadow should be \
             known after the first passes"
        );
        // And everything above ground in the cylinder WAS sent — sealing must
        // never hide a chunk the player could see.
        for pos in interest::chunks_around(ORIGIN, TALL) {
            if pos.y >= ORIGIN.y {
                assert!(
                    all.contains(&pos),
                    "an open chunk at {pos:?} was never requested"
                );
            }
        }
        // Non-vacuous: rock was actually excluded, not merely absent from a
        // tiny radius.
        let in_radius = interest::chunks_around(ORIGIN, TALL).len();
        assert!(
            all.len() < in_radius,
            "every position in the radius was sent, so sealing excluded nothing"
        );
    }

    #[test]
    fn digging_into_a_sealed_chunk_opens_what_lay_behind_it() {
        // A player standing on rock digs down. The chunk they dig into is not
        // uniform any more, the edit reaches every connection as a delta, and
        // the streamer reopens it — so the chunk below, which was never sent,
        // is requested next. This is what keeps sealing from being a hole the
        // player can fall into.
        let mut streamer = tall_streamer();
        let floor = ORIGIN.y - 1;
        let all = stream_all(&mut streamer, |pos| pos.y <= floor);
        // The deepest rock sent in the player's own column is the shell as it
        // stands — the floor, or one under it if the first passes asked before
        // the floor had arrived. The chunk beneath THAT is what digging opens.
        let shell_y = all
            .iter()
            .filter(|pos| pos.x == ORIGIN.x && pos.z == ORIGIN.z)
            .map(|pos| pos.y)
            .min()
            .expect("the column under the player was sent");
        let below = ChunkPos::new(ORIGIN.x, shell_y - 1, ORIGIN.z);
        assert!(
            !streamer.next_needed(usize::MAX).contains(&below),
            "the chunk under the shell was requested before anything was dug"
        );

        streamer.unseal(ChunkPos::new(ORIGIN.x, shell_y, ORIGIN.z));
        let needed = streamer.next_needed(usize::MAX);
        assert!(
            needed.contains(&below),
            "digging into the floor did not make the chunk beneath it requestable: {needed:?}"
        );
        // Only what is behind the hole: the rock two columns over stays sealed
        // and what is under IT stays unrequested.
        let far_below = ChunkPos::new(ORIGIN.x + 2, floor - 2, ORIGIN.z);
        assert!(
            !needed.contains(&far_below),
            "a hole in one chunk reopened rock it does not touch"
        );
    }

    #[test]
    fn a_cave_under_a_roof_is_reached_from_the_side() {
        // The case that rules out "skip whatever is under sealed rock": a
        // pocket of open chunks under a sealed roof, joined to the surface by
        // one open column at the edge. The shaft is open, so the pocket chunk
        // beside its foot has an open neighbour and is sent, and then the one
        // beside THAT — the known-open-neighbour escape in `is_shadowed` walks
        // the pocket; a rule about roofs would never send it.
        let mut streamer = tall_streamer();
        let floor = ORIGIN.y - 1;
        let shaft_x = ORIGIN.x + 1;
        let wall = |pos: ChunkPos| {
            let in_shaft = pos.x == shaft_x && pos.z == ORIGIN.z;
            let in_pocket = pos.y == floor - 2 && pos.z == ORIGIN.z && pos.x <= shaft_x;
            pos.y <= floor && !in_shaft && !in_pocket
        };
        let all = stream_all(&mut streamer, wall);
        let pocket_end = ChunkPos::new(ORIGIN.x - 1, floor - 2, ORIGIN.z);
        assert!(
            all.contains(&pocket_end),
            "the cave under the roof was never sent: {all:?}"
        );
        // And the rock BELOW the pocket's floor was sealed and stopped there.
        // Two below the cave floor: inside the cylinder (vertical 5), adjacent
        // only to sealed rock, and so never asked for.
        let under_pocket = ChunkPos::new(ORIGIN.x - 1, floor - 4, ORIGIN.z);
        assert!(
            !all.contains(&under_pocket),
            "sealing stopped nothing under the cave floor"
        );
    }

    #[test]
    fn moving_domain_takes_back_every_chunk_and_not_just_the_ones_out_of_range() {
        // **The whole set, because none of it means anything any more.** The
        // client holds chunks belonging to the space it is leaving, and every
        // one of their positions names a different chunk in the space it is
        // entering. A switch that only returned what left range would leave
        // terrain from somewhere else standing exactly where the player is.
        let mut streamer = streamer();
        let needed = streamer.next_needed(8);
        assert!(!needed.is_empty(), "nothing to send makes this vacuous");
        for pos in &needed {
            streamer.requested(*pos);
            streamer.delivered(*pos);
        }
        assert_eq!(streamer.sent_count(), needed.len());

        let dropped = streamer.switch_to("mod:ship/17", ORIGIN);
        assert_eq!(
            dropped.len(),
            needed.len(),
            "a domain switch kept chunks the client can no longer use"
        );
        assert_eq!(streamer.sent_count(), 0);
        assert_eq!(streamer.domain(), "mod:ship/17");
    }

    #[test]
    fn a_request_outstanding_when_the_domain_changes_is_abandoned() {
        // A reply carrying a chunk of the domain just left would be decoded
        // into the new one at the same coordinates — terrain from somewhere
        // else, in a place somebody is standing.
        let mut streamer = streamer();
        let pos = streamer.next_needed(1)[0];
        streamer.requested(pos);
        assert_eq!(streamer.in_flight(), 1);

        streamer.switch_to("mod:ship/17", ORIGIN);
        assert_eq!(
            streamer.in_flight(),
            0,
            "a chunk of the old domain was still expected after the move"
        );
        assert!(
            streamer.next_needed(usize::MAX).contains(&pos),
            "the new domain's chunk at that position was never asked for"
        );
    }

    #[test]
    fn switching_to_the_domain_already_being_streamed_costs_nothing() {
        // The connection checks every tick, so the common answer has to be
        // cheap and has to change nothing.
        let mut streamer = streamer();
        let pos = streamer.next_needed(1)[0];
        streamer.requested(pos);
        streamer.delivered(pos);

        let dropped = streamer.switch_to(tiamat_core::domain::OVERWORLD, ORIGIN);
        assert!(dropped.is_empty(), "a no-op switch threw the world away");
        assert_eq!(streamer.sent_count(), 1);
    }

    #[test]
    fn shrinking_the_radius_unloads_what_left_range() {
        // The client asking to see less has to actually cost less, and the
        // chunks it can no longer see have to be taken off it — otherwise
        // "reduce your view distance" would free nothing on either side, which
        // is the entire reason somebody reaches for the setting.
        //
        // **What "range" means changed with Task 15b.** A chunk that falls out
        // of the detail radius has not left the client's world; it becomes a
        // summary. Only the horizon unloads.
        let mut streamer = Streamer::new(
            tiamat_core::domain::OVERWORLD,
            ORIGIN,
            ViewDistance::DEFAULT,
        );
        loop {
            let needed = streamer.next_needed(usize::MAX);
            if needed.is_empty() {
                break;
            }
            for pos in needed {
                streamer.requested(pos);
                streamer.completed(pos);
                streamer.delivered(pos);
            }
        }
        let before = streamer.sent_count();
        assert!(streamer.is_complete());

        let departed = streamer.resize(ViewDistance::MINIMUM);
        assert!(
            !departed.is_empty(),
            "shrinking to the minimum unloaded nothing"
        );
        assert_eq!(
            streamer.sent_count(),
            before - departed.len(),
            "the unload list and what is still held must agree"
        );
        assert_eq!(
            streamer.sent_count(),
            interest::chunks_around(ORIGIN, horizon_for(ViewDistance::MINIMUM)).len(),
            "what is left should be exactly the smaller horizon"
        );
        for pos in &departed {
            assert!(
                !interest::contains(ORIGIN, horizon_for(ViewDistance::MINIMUM), *pos),
                "{pos:?} was unloaded but is still inside the horizon"
            );
        }
    }

    #[test]
    fn growing_the_radius_unloads_nothing_and_asks_for_the_rest() {
        // No chunk leaves range when the range gets bigger, and the new ones
        // arrive through the ordinary pump rather than through a special path.
        let mut streamer = Streamer::new(
            tiamat_core::domain::OVERWORLD,
            ORIGIN,
            ViewDistance::MINIMUM,
        );
        loop {
            let needed = streamer.next_needed(usize::MAX);
            if needed.is_empty() {
                break;
            }
            for pos in needed {
                streamer.requested(pos);
                streamer.completed(pos);
                streamer.delivered(pos);
            }
        }
        let held = streamer.sent_count();

        assert!(
            streamer.resize(ViewDistance::DEFAULT).is_empty(),
            "growing the radius unloaded something"
        );
        assert_eq!(streamer.sent_count(), held, "growing dropped a held chunk");
        assert!(
            !streamer.is_complete(),
            "growing the radius asked for nothing new"
        );
    }

    #[test]
    fn resizing_to_the_same_radius_is_a_no_op() {
        // A client re-sending its preference — on a reconnect, or every time a
        // settings screen closes — must not churn its whole interest set.
        let mut streamer = Streamer::new(
            tiamat_core::domain::OVERWORLD,
            ORIGIN,
            ViewDistance::DEFAULT,
        );
        loop {
            let needed = streamer.next_needed(usize::MAX);
            if needed.is_empty() {
                break;
            }
            for pos in needed {
                streamer.requested(pos);
                streamer.completed(pos);
                streamer.delivered(pos);
            }
        }
        assert!(streamer.resize(ViewDistance::DEFAULT).is_empty());
        assert!(streamer.is_complete());
    }

    #[test]
    fn a_fresh_streamer_needs_its_whole_interest_set() {
        // Nothing is sealed yet, so nothing is in shadow, so everything in
        // range is asked for — nearest first. Sealing only ever removes
        // positions from this order; it never holds one back for another.
        let mut streamer = streamer();
        let needed = streamer.next_needed(usize::MAX);
        assert_eq!(
            needed.len(),
            interest::chunks_around(ORIGIN, ViewDistance::MINIMUM).len()
        );
        assert_eq!(needed[0], ORIGIN, "nearest first");
        assert!(!streamer.is_complete());
    }

    #[test]
    fn delivered_chunks_are_not_requested_again() {
        let mut streamer = streamer();
        let first = streamer.next_needed(3);
        for pos in &first {
            streamer.delivered(*pos);
        }

        let second = streamer.next_needed(usize::MAX);
        for pos in &first {
            assert!(
                !second.contains(pos),
                "{pos:?} was delivered and must not be needed again"
            );
        }
    }

    #[test]
    fn a_streamer_becomes_complete_once_everything_is_delivered() {
        let mut streamer = streamer();
        deliver_all(&mut streamer);
        assert!(streamer.is_complete());
        assert!(streamer.next_needed(usize::MAX).is_empty());
    }

    #[test]
    fn the_limit_is_respected() {
        let mut streamer = streamer();
        assert_eq!(streamer.next_needed(2).len(), 2);
        assert_eq!(streamer.next_needed(0).len(), 0);
    }

    #[test]
    fn moving_unloads_what_left_range_and_loads_what_entered() {
        let mut streamer = streamer();
        deliver_all(&mut streamer);
        assert!(streamer.is_complete());

        // Far enough east that the whole original neighbourhood is outside the
        // HORIZON, not merely outside the detail radius — the second only turns
        // a chunk into a summary, which is not an unload.
        let away = ChunkPos::new(32, 0, 0);
        let departed = streamer.recentre(away);

        assert!(!departed.is_empty(), "moving away must unload something");
        for pos in &departed {
            assert!(
                !interest::contains(away, streamer.horizon(), *pos),
                "{pos:?} was unloaded but is still inside the horizon"
            );
        }
        assert!(
            !streamer.is_complete(),
            "the new neighbourhood still needs streaming"
        );
    }

    #[test]
    fn an_unloaded_chunk_is_streamed_again_on_return() {
        // A client told to unload a chunk has thrown it away. Remembering that
        // it once had it would leave a hole in the world when the player walked
        // back.
        let mut streamer = streamer();
        deliver_all(&mut streamer);

        let departed = streamer.recentre(ChunkPos::new(5, 0, 0));
        let returned = streamer.recentre(ORIGIN);
        // Across rounds: the way back is streamed in layers like the way out.
        let mut needed = BTreeSet::new();
        loop {
            let round = streamer.next_needed(usize::MAX);
            if round.is_empty() {
                break;
            }
            for pos in round {
                needed.insert(pos);
                streamer.delivered(pos);
            }
        }

        assert!(
            departed.iter().all(|pos| needed.contains(pos)),
            "chunks unloaded on the way out must be re-sent on the way back"
        );
        let _ = returned;
    }

    #[test]
    fn recentring_to_the_same_place_changes_nothing() {
        let mut streamer = streamer();
        deliver_all(&mut streamer);
        assert!(streamer.recentre(ORIGIN).is_empty());
        assert!(streamer.is_complete(), "a no-op move must not re-stream");
    }

    #[test]
    fn moving_within_range_keeps_the_overlap() {
        // A player taking one step must not be re-sent their whole
        // neighbourhood. If this failed, walking would saturate the link.
        let mut streamer = Streamer::new(
            tiamat_core::domain::OVERWORLD,
            ORIGIN,
            ViewDistance::DEFAULT,
        );
        deliver_all(&mut streamer);
        let before = streamer.sent_count();

        let departed = streamer.recentre(ChunkPos::new(1, 0, 0));

        assert!(
            departed.len() < before / 4,
            "one chunk of movement unloaded {} of {before} chunks, which means the \
             overlap is not being kept",
            departed.len()
        );
        assert!(
            streamer.sent_count() > before / 2,
            "most of the neighbourhood should have been retained"
        );
    }

    #[test]
    fn a_chunk_in_flight_is_not_requested_again() {
        // The bug macOS CI caught. `next_needed` used to filter only against
        // what had been DELIVERED, so a chunk still in flight looked
        // un-requested and was asked for a second time — and the client
        // received it twice. On a fast machine the reply landed before the next
        // pass and it never showed.
        let mut streamer = streamer();

        let first = streamer.next_needed(2);
        assert_eq!(first.len(), 2);
        for pos in &first {
            streamer.requested(*pos);
        }

        let second = streamer.next_needed(usize::MAX);
        for pos in &first {
            assert!(
                !second.contains(pos),
                "{pos:?} is in flight and must not be requested again"
            );
        }
    }

    #[test]
    fn in_flight_accounting_survives_a_failed_request() {
        // If `completed` were only called on success, dropped requests would
        // hold their slots permanently and the player's world would simply stop
        // filling in — with nothing logged.
        let mut streamer = streamer();
        assert_eq!(streamer.budget(4), 4);

        let targets = streamer.next_needed(2);
        for pos in &targets {
            streamer.requested(*pos);
        }
        assert_eq!(streamer.budget(4), 2);

        // One delivered, one failed.
        streamer.delivered(targets[0]);
        streamer.completed(targets[1]);

        assert_eq!(
            streamer.budget(4),
            4,
            "a failed request must return its slot too"
        );
        assert!(
            streamer.next_needed(usize::MAX).contains(&targets[1]),
            "the failed chunk must be retried"
        );
        assert!(
            !streamer.next_needed(usize::MAX).contains(&targets[0]),
            "the delivered chunk must not be"
        );
    }

    #[test]
    fn completing_something_never_requested_is_harmless() {
        let mut streamer = streamer();
        streamer.completed(ORIGIN);
        streamer.completed(ORIGIN);
        assert_eq!(streamer.in_flight(), 0);
        assert_eq!(streamer.budget(4), 4);
    }

    #[test]
    fn moving_away_abandons_requests_for_chunks_that_left_range() {
        // Otherwise the reply arrives for a chunk the client was told to
        // unload, and the slot it holds is one the new neighbourhood needs.
        let mut streamer = streamer();
        let targets = streamer.next_needed(3);
        for pos in &targets {
            streamer.requested(*pos);
        }
        assert_eq!(streamer.in_flight(), 3);

        streamer.recentre(ChunkPos::new(20, 0, 0));
        assert_eq!(
            streamer.in_flight(),
            0,
            "requests for chunks now out of range must be abandoned"
        );
        assert_eq!(streamer.budget(4), 4);
    }

    #[test]
    fn holds_reports_what_the_client_actually_has() {
        let mut streamer = streamer();
        assert!(!streamer.holds(ORIGIN));
        streamer.delivered(ORIGIN);
        assert!(streamer.holds(ORIGIN));

        streamer.recentre(ChunkPos::new(9, 0, 0));
        assert!(
            !streamer.holds(ORIGIN),
            "an unloaded chunk is no longer held"
        );
    }

    #[test]
    fn the_horizon_starts_where_the_detail_radius_ends_and_never_overlaps_it() {
        // A client holding a chunk AND a summary of it would draw both, and the
        // coarse one would poke through the fine one. The two sets are disjoint
        // by construction, and this is the assertion that keeps them so.
        let mut streamer = Streamer::new(
            tiamat_core::domain::OVERWORLD,
            ORIGIN,
            ViewDistance::DEFAULT,
        );
        deliver_all(&mut streamer);
        for (pos, level) in drain_horizon(&mut streamer) {
            assert!(
                !streamer.holds(pos),
                "{pos:?} was to be summarised while the client held it in full"
            );
            assert!(level >= tiamat_core::lod::FINEST);
        }
        assert!(streamer.summary_count() > 0, "no horizon was produced");
        assert!(
            streamer.next_summaries(usize::MAX).is_empty(),
            "a horizon already sent was asked for a second time"
        );
    }

    #[test]
    fn the_horizon_waits_behind_the_detail_and_fills_in_step_with_it() {
        // Reported from the window: a ring of chunks far out loading with the
        // spawn chunk, and a gap between. The ring was the horizon's nearest
        // band, asked for on its own allowance from the first pass while the
        // detail was still filling in from the player's feet outward. The
        // horizon may ask only for what is no further, in streaming order,
        // than the nearest chunk the detail has yet to send.
        let mut streamer = Streamer::new(
            tiamat_core::domain::OVERWORLD,
            ORIGIN,
            ViewDistance::DEFAULT,
        );
        // The first pass asks for the chunks under the player and nothing
        // of the horizon.
        assert!(!streamer.next_needed(16).is_empty());
        assert_eq!(streamer.frontier(), Some(0));
        assert!(
            streamer.next_summaries(8).is_empty(),
            "the horizon ran ahead of the detail"
        );

        // Deliver the detail out to a hundred in streaming order — the band
        // the player looks along, to the radius, and a little of what is
        // above and below — leaving the rest needed.
        while let Some(pos) = streamer.next_needed(1).into_iter().next() {
            if interest::stream_distance(ORIGIN, pos) > 100 {
                break;
            }
            streamer.delivered(pos);
        }
        let frontier = streamer.frontier().expect("the detail is not complete");
        assert!(frontier > 100, "frontier {frontier}");

        // Now the horizon flows, but only up to the frontier: the summaries
        // just past the detail radius on the player's own layer, and nothing
        // further out than a chunk the detail still owes.
        let asked = streamer.next_summaries(64);
        assert!(
            !asked.is_empty(),
            "the horizon was held back past the frontier"
        );
        for (pos, _) in &asked {
            let distance = interest::stream_distance(ORIGIN, *pos);
            assert!(
                distance <= frontier,
                "a summary at {pos:?} (streaming distance {distance}) was asked for past \
                 the detail's frontier at {frontier}"
            );
        }

        // With the detail complete the horizon has the sky to itself.
        deliver_all(&mut streamer);
        assert_eq!(streamer.frontier(), None);
        let rest = drain_horizon(&mut streamer);
        assert!(
            rest.len() > asked.len(),
            "the horizon stopped filling once the detail was complete"
        );
    }

    #[test]
    fn walking_forward_turns_a_summary_into_a_chunk_and_back_without_an_unload() {
        // The transition a player actually experiences. Neither direction is an
        // unload: a summary replaced by a chunk, and a chunk replaced by a
        // summary, are both one message about one position. Unloading first
        // would blink a chunk-sized hole in the world.
        let mut streamer = Streamer::new(
            tiamat_core::domain::OVERWORLD,
            ORIGIN,
            ViewDistance::DEFAULT,
        );
        let far = ChunkPos::new(12, 0, 0);
        let level = drain_horizon(&mut streamer)
            .into_iter()
            .find_map(|(pos, level)| (pos == far).then_some(level))
            .expect("a chunk twelve out should be summarised at a default view of eight");
        assert_eq!(streamer.summary_level(far), Some(level));

        // Walk towards it until it is inside the detail radius.
        let departed = streamer.recentre(ChunkPos::new(8, 0, 0));
        assert!(
            !departed.contains(&far),
            "walking towards a chunk unloaded it"
        );
        // In rounds: a delivery can lift a shadow, so what is asked for after
        // a move is not always settled in one pass.
        let mut asked = BTreeSet::new();
        loop {
            let round = streamer.next_needed(usize::MAX);
            if round.is_empty() {
                break;
            }
            for pos in round {
                asked.insert(pos);
                streamer.delivered(pos);
            }
        }
        assert!(
            asked.contains(&far),
            "a chunk that came inside the detail radius was not asked for in full"
        );
        assert_eq!(
            streamer.summary_level(far),
            None,
            "the client was left holding a summary of a chunk it now has in full"
        );

        // And back out again.
        let departed = streamer.recentre(ORIGIN);
        assert!(!departed.contains(&far), "walking away unloaded it");
        assert!(
            drain_horizon(&mut streamer)
                .iter()
                .any(|(at, _)| *at == far),
            "a chunk that left the detail radius was not re-sent as a summary"
        );
    }

    #[test]
    fn a_player_pacing_across_a_ring_edge_is_sent_nothing() {
        // **Criterion T3, at the level that costs bandwidth rather than
        // frames.** The client's rebuild count is downstream of this: a
        // summary it is not sent is one it cannot rebuild.
        let mut streamer = Streamer::new(
            tiamat_core::domain::OVERWORLD,
            ORIGIN,
            ViewDistance::DEFAULT,
        );
        deliver_all(&mut streamer);
        drain_horizon(&mut streamer);

        // Step back and forth across the level-1/level-2 edge, which at a
        // detail radius of eight sits sixteen chunks out.
        let mut sends = 0;
        for step in 0..20 {
            let centre = if step % 2 == 0 {
                ChunkPos::new(0, 0, 0)
            } else {
                ChunkPos::new(1, 0, 0)
            };
            streamer.recentre(centre);
            for _ in 0..64 {
                let batch = streamer.next_summaries(usize::MAX);
                for (pos, level) in batch {
                    // Chunks genuinely entering the horizon for the first time
                    // are not churn — count only re-sends of what is held.
                    if streamer.summary_level(pos).is_some() {
                        sends += 1;
                    }
                    streamer.summarised(pos, level);
                }
            }
        }
        assert_eq!(
            sends, 0,
            "pacing one chunk back and forth re-sent {sends} summaries the client \
             already held"
        );
    }

    #[test]
    fn an_edit_in_a_summarised_chunk_re_sends_the_summary() {
        // A block delta is no use to a client holding a summary: it has nowhere
        // to put one cell out of twenty-seven. The horizon is re-sent instead,
        // and this is what makes a distant explosion eventually show up.
        let mut streamer = Streamer::new(
            tiamat_core::domain::OVERWORLD,
            ORIGIN,
            ViewDistance::DEFAULT,
        );
        let far = ChunkPos::new(12, 0, 0);
        assert!(
            drain_horizon(&mut streamer)
                .iter()
                .any(|(at, _)| *at == far),
            "a summarised chunk"
        );
        assert!(
            !drain_horizon(&mut streamer)
                .iter()
                .any(|(at, _)| *at == far)
        );

        streamer.resummarise(far);
        assert!(
            drain_horizon(&mut streamer)
                .iter()
                .any(|(at, _)| *at == far),
            "an edited chunk's horizon was never sent again"
        );
    }

    #[test]
    fn the_horizon_starts_before_the_detail_radius_has_finished_arriving() {
        // **The bug this exists for.** The horizon used to be asked for with
        // whatever in-flight budget the chunks had left, which on any real view
        // distance is nothing: a client streaming thousands of chunks takes the
        // whole allowance every pass for as long as that lasts. Reported from
        // the window at view 17 as "horizon 32: 0 held" after a thousand ticks.
        //
        // Priority is still the point — the ground under somebody's feet is not
        // scenery — so this asserts only that the horizon is not starved to
        // zero, not that it competes.
        //
        // **And since 2026-09-23, not before the detail has reached it.** It
        // used to start with nothing delivered at all, and that was the ring
        // of summaries far out that loaded with the spawn chunk. So: the
        // player's own layer delivered out to the radius, most of the detail
        // — the sky above and the rock below — still to come, and the horizon
        // must flow anyway. See `the_horizon_waits_behind_the_detail...`.
        let mut streamer = Streamer::new(
            tiamat_core::domain::OVERWORLD,
            ORIGIN,
            ViewDistance::DEFAULT,
        );
        while let Some(pos) = streamer.next_needed(1).into_iter().next() {
            // The first ring of the horizon is nine out on the player's own
            // layer, a streaming distance of 81.
            if interest::stream_distance(ORIGIN, pos) > 81 {
                break;
            }
            streamer.delivered(pos);
        }
        let outstanding = streamer.next_needed(usize::MAX).len();
        assert!(
            outstanding > 1000,
            "the detail radius should still be mostly outstanding, {outstanding} left"
        );
        assert!(
            !streamer.next_summaries(1).is_empty(),
            "the horizon was starved while the detail radius streamed"
        );
    }

    #[test]
    fn one_pass_over_the_horizon_looks_at_a_bounded_number_of_positions() {
        // The horizon is 38,000 positions at the default view, and this runs on
        // the connection task every pass. A scan that walked all of them — or,
        // worse, rebuilt and re-sorted them — would cost more than the feature
        // saves. The cursor rotates instead, so everything gets a turn without
        // any single pass paying for all of it.
        let mut streamer = Streamer::new(
            tiamat_core::domain::OVERWORLD,
            ORIGIN,
            ViewDistance::DEFAULT,
        );
        let total = interest::chunks_around(ORIGIN, streamer.horizon()).len();
        assert!(
            total > HORIZON_SCAN,
            "the default horizon should be bigger than one scan, got {total}"
        );

        // Asking for everything cannot return everything, because one pass does
        // not look at everything.
        assert!(streamer.next_summaries(usize::MAX).len() <= HORIZON_SCAN);

        // But the cursor comes round: enough passes cover the whole horizon.
        let found = drain_horizon(&mut streamer);
        assert!(
            found.len() > HORIZON_SCAN,
            "rotating the cursor should reach past one scan's worth, got {}",
            found.len()
        );
    }

    #[test]
    fn a_streamer_is_not_complete_while_requests_are_outstanding() {
        // Otherwise a connection would decide it had finished streaming while
        // chunks were still on their way, and stop asking.
        let mut streamer = streamer();
        deliver_all(&mut streamer);
        streamer.requested(ORIGIN);
        assert!(!streamer.is_complete());
        streamer.completed(ORIGIN);
        assert!(streamer.is_complete());
    }

    #[test]
    fn a_sealed_arrival_keeps_the_shadow_memo_and_an_open_one_lifts_only_its_own() {
        // The memo is what keeps a pass cheap, so what it keeps and what it
        // drops is the contract. Rock below a floor is shadowed; a chunk of sky
        // arriving above the player is in no rock's box and drops nothing; a
        // sealed chunk arriving drops nothing; digging into the floor drops
        // exactly what lay in the box between the hole and the centre.
        let mut streamer = tall_streamer();
        let floor = ORIGIN.y - 1;
        stream_all(&mut streamer, |pos| pos.y <= floor);
        let shadowed_before = streamer.shadowed.len();
        assert!(
            shadowed_before > 0,
            "nothing was memoised, so this measures nothing"
        );

        streamer.requested(ChunkPos::new(ORIGIN.x + 3, ORIGIN.y + 2, ORIGIN.z));
        streamer.delivered(ChunkPos::new(ORIGIN.x + 3, ORIGIN.y + 2, ORIGIN.z));
        assert_eq!(
            streamer.shadowed.len(),
            shadowed_before,
            "sky lifted a shadow"
        );

        streamer.delivered_sealed(ChunkPos::new(ORIGIN.x + 3, floor - 3, ORIGIN.z));
        assert_eq!(
            streamer.shadowed.len(),
            shadowed_before,
            "a wall lifted a shadow"
        );

        // A hole one column over: what is under it is dropped, and rock on the
        // far side of the player — whose walk toward the centre never comes
        // near the hole — is kept. (A hole in the player's own column would
        // drop every entry, because every walk ends in that column: the box
        // test is a cheap over-approximation, and that case is rare.)
        streamer.unseal(ChunkPos::new(ORIGIN.x + 1, floor, ORIGIN.z));
        let under = ChunkPos::new(ORIGIN.x + 1, floor - 1, ORIGIN.z);
        assert!(
            !streamer.shadowed.contains(&under),
            "the chunk under the hole is still memoised as shadowed"
        );
        let far = ChunkPos::new(ORIGIN.x - 1, floor - 3, ORIGIN.z);
        assert!(
            streamer.shadowed.contains(&far),
            "rock on the far side of the player was dropped from the memo"
        );
    }

    /// **Ignored: it measures rather than asserts.** Run by hand — `cargo test
    /// -p server --release measure_a_pass -- --ignored --nocapture` — when
    /// touching the shadow rule, and put the number in the memo's doc.
    #[test]
    #[ignore = "measures rather than asserts; run by hand"]
    fn measure_a_pass_at_the_default_view() {
        let centre = ChunkPos::new(0, 0, 0);
        let mut streamer = Streamer::new("overworld", centre, ViewDistance::DEFAULT);
        let join = std::time::Instant::now();
        let sent = stream_all(&mut streamer, |pos| pos.y < centre.y);
        println!(
            "a whole join, eight a pass, {} passes: {:?} of streamer time",
            sent.len().div_ceil(8),
            join.elapsed()
        );
        let passes = 100u32;
        let cold = std::time::Instant::now();
        for _ in 0..passes {
            streamer.shadowed.clear();
            streamer.next_needed(8);
        }
        let cold = cold.elapsed() / passes;
        let warm = std::time::Instant::now();
        for _ in 0..passes {
            streamer.next_needed(8);
        }
        let warm = warm.elapsed() / passes;
        println!(
            "a pass with the memo cleared: {cold:?}; with it kept: {warm:?}; {} of {} sent",
            streamer.sent.len(),
            interest::chunks_around(centre, ViewDistance::DEFAULT).len()
        );
    }
}
