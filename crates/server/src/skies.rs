// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! The skies a world's mods registered: one for every domain not named, and
//! one per domain that was.
//!
//! A space between worlds has no dawn, and a body a player lands on has a sky
//! of its own, so `register_sky` takes a `domain` and this is where the
//! server keeps the answer to "which sky does this domain get". The rule:
//! the domain's own, else — for an instance — its template's, else the sky
//! for every domain not named, else none.
//!
//! **One clock.** The world has a day, not one per domain: the tick advances a
//! single time of day and every sky is interpolated from it. So every table
//! sent carries the same day length, the one the sky for every domain not
//! named declared (or, in a world with only domain skies, the first of them),
//! whatever a domain sky wrote in its own — a body whose day was twice as long
//! as home's would need the client to keep a second clock, and it keeps one.

use std::collections::BTreeMap;

use tiamat_core::proto::{ServerMessage, SkyFrame, SkyGrade};
use tiamat_core::script::Sky;
use tiamat_core::sky::UniversalPos;

/// One registered sky, as the wire carries its keyframes.
#[derive(Debug, Clone, PartialEq)]
pub struct SkyWire {
    /// Colour keyframes, sorted by time and never empty.
    pub keyframes: Vec<SkyFrame>,
    /// Where a fresh world's clock starts, `0.0..1.0`.
    pub start_time: f32,
}

/// Every sky a world has, by domain.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SkyBook {
    /// The world's one day, in ticks. Zero when no sky at all was registered.
    day_length_ticks: u32,
    /// The sky for every domain not named.
    default: Option<SkyWire>,
    /// The skies for the domains that were named.
    by_domain: BTreeMap<String, SkyWire>,
}

impl SkyBook {
    /// The book for what the mods registered.
    #[must_use]
    pub fn from_registered(skies: Vec<Sky>) -> Self {
        let mut book = Self::default();
        for sky in skies {
            let wire = SkyWire {
                keyframes: sky.keyframes.iter().map(frame_on_the_wire).collect(),
                start_time: sky.start_time,
            };
            if book.day_length_ticks == 0 || sky.domain.is_none() {
                book.day_length_ticks = sky.day_length_ticks;
            }
            match sky.domain {
                None => book.default = Some(wire),
                Some(domain) => {
                    book.by_domain.insert(domain, wire);
                }
            }
        }
        book
    }

    /// Ticks in the world's one day, or zero for a world with no sky.
    #[must_use]
    pub const fn day_length_ticks(&self) -> u32 {
        self.day_length_ticks
    }

    /// Where a fresh world's clock starts, `0.0..1.0`: the sky for every
    /// domain not named decides, or the first domain sky in a world with only
    /// those, or midnight in a world with none.
    #[must_use]
    pub fn start_time(&self) -> f32 {
        self.default
            .as_ref()
            .or_else(|| self.by_domain.values().next())
            .map_or(0.0, |wire| wire.start_time)
    }

    /// Whether any sky at all was registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.default.is_none() && self.by_domain.is_empty()
    }

    /// The sky a domain gets, by the module's rule, or `None` for no sky.
    #[must_use]
    pub fn wire_for(&self, domain: &str) -> Option<&SkyWire> {
        if let Some(own) = self.by_domain.get(domain) {
            return Some(own);
        }
        if let Some((template, _)) = domain.split_once(tiamat_core::domain::INSTANCE_SEPARATOR)
            && let Some(inherited) = self.by_domain.get(template)
        {
            return Some(inherited);
        }
        self.default.as_ref()
    }

    /// The day length and keyframes a domain's table carries: the world's
    /// day and the domain's frames, or zero and none for no sky.
    #[must_use]
    pub fn frames_for(&self, domain: &str) -> (u32, &[SkyFrame]) {
        self.wire_for(domain).map_or((0, &[][..]), |wire| {
            (self.day_length_ticks, &wire.keyframes)
        })
    }

    /// The table to send for a domain seen from `observer`.
    #[must_use]
    pub fn table_for(&self, domain: &str, observer: UniversalPos) -> ServerMessage {
        let (day_length_ticks, keyframes) = self.frames_for(domain);
        ServerMessage::SkyTable {
            day_length_ticks,
            keyframes: keyframes.to_vec(),
            observer: [observer.x, observer.y, observer.z],
        }
    }
}

/// A mod's keyframe as peers agree on it.
fn frame_on_the_wire(frame: &tiamat_core::script::SkyKeyframe) -> SkyFrame {
    SkyFrame {
        time: frame.time,
        sky: frame.sky,
        sun: frame.sun,
        intensity: frame.intensity,
        grade: SkyGrade {
            exposure: frame.grade.exposure,
            tint: frame.grade.tint,
            offset: frame.grade.offset,
            contrast: frame.grade.contrast,
            saturation: frame.grade.saturation,
            gamma: frame.grade.gamma,
        },
        stars: frame.stars,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiamat_core::script::SkyKeyframe;

    fn sky(domain: Option<&str>, day: u32, stars: f32) -> Sky {
        Sky {
            mod_id: "space".to_owned(),
            domain: domain.map(str::to_owned),
            day_length_ticks: day,
            keyframes: vec![SkyKeyframe {
                time: 0.5,
                sky: [0.1, 0.2, 0.3],
                sun: [1.0; 3],
                intensity: 1.0,
                grade: tiamat_core::script::SkyGrade::NONE,
                stars,
            }],
            start_time: 0.4,
        }
    }

    #[test]
    fn a_domain_gets_its_own_sky_an_instance_its_templates_and_the_rest_the_default() {
        let book = SkyBook::from_registered(vec![
            sky(None, 24_000, 0.0),
            sky(Some("space:void"), 100, 1.0),
            sky(Some("space:body"), 200, 0.5),
        ]);
        let stars = |domain: &str| book.frames_for(domain).1[0].stars;
        assert!((stars("overworld") - 0.0).abs() < f32::EPSILON);
        assert!((stars("space:void") - 1.0).abs() < f32::EPSILON);
        assert!(
            (stars("space:body/17") - 0.5).abs() < f32::EPSILON,
            "an instance inherits"
        );
        assert!(
            (stars("space:cellar") - 0.0).abs() < f32::EPSILON,
            "the rest get the default"
        );
        // One clock: every table carries the default's day.
        assert_eq!(book.frames_for("space:void").0, 24_000);
        assert_eq!(book.day_length_ticks(), 24_000);
    }

    #[test]
    fn a_world_with_only_domain_skies_still_has_a_day_and_the_rest_have_none() {
        let book = SkyBook::from_registered(vec![sky(Some("space:void"), 100, 1.0)]);
        assert_eq!(book.day_length_ticks(), 100);
        assert_eq!(book.frames_for("overworld"), (0, &[][..]));
        assert!((book.start_time() - 0.4).abs() < f32::EPSILON);
        assert!(matches!(
            book.table_for("overworld", UniversalPos::new(1, 2, 3)),
            ServerMessage::SkyTable {
                day_length_ticks: 0,
                observer: [1, 2, 3],
                ..
            }
        ));
    }

    #[test]
    fn no_sky_at_all_is_a_world_without_a_day() {
        let book = SkyBook::from_registered(Vec::new());
        assert!(book.is_empty());
        assert_eq!(book.day_length_ticks(), 0);
        assert!((book.start_time() - 0.0).abs() < f32::EPSILON);
    }
}
