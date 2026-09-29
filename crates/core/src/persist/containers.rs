// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Inventories that belong to the world rather than to a player.
//!
//! A chest, a furnace, a hopper. The engine owns the slots — stacking,
//! conservation and the material id map are all its rules — and a mod owns what
//! the container MEANS: where it is, what may go in it, who may open it.
//!
//! # Why this is not `game.storage`
//!
//! A mod could serialise its own chests into its own key-value store. It would
//! then be reimplementing stacking, unit conservation and the string-to-numeric
//! id map (charter rule 8), and getting one of the three wrong somewhere the
//! engine would have got it right. What a mod cannot express is exactly what
//! this is for.
//!
//! # The id map, again
//!
//! Stored in WORLD ids like everything else that reaches disk, and translated
//! on the way in and out. A runtime id in a save is the fluid defect (`7dc37d8`)
//! waiting to happen: still a valid number, and the wrong material, the day a
//! mod's load order changes.

use serde::{Deserialize, Serialize};

use crate::inventory::{Shape, View};
use crate::material::MaterialId;
use crate::persist::cutcells;
use crate::persist::idmap::MaterialMap;

/// The version this build writes.
///
/// Bumped for any change to what is stored. An older version is migrated
/// rather than refused — see [`decode`].
///
/// 2: a stack may be a cut of several materials, and carries its cells.
pub const CONTAINER_FORMAT_VERSION: u8 = 2;

/// One stack, in world ids.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredStack {
    material: u16,
    units: u32,
    shape: Option<u32>,
    detail: Option<String>,
    /// Each cell's material, in WORLD ids with `0` for air, for a cut of
    /// several materials (format v2, Sub-Node Contract §9.1).
    cells: Option<Vec<u16>>,
}

/// A stack as format v1 wrote one: before a stack could hold several
/// materials. A copy of the struct as it was, not the live one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredStackV1 {
    material: u16,
    units: u32,
    shape: Option<u32>,
    detail: Option<String>,
}

impl From<StoredStackV1> for StoredStack {
    fn from(old: StoredStackV1) -> Self {
        Self {
            material: old.material,
            units: old.units,
            shape: old.shape,
            detail: old.detail,
            // Nothing written before v2 could be made of several materials.
            cells: None,
        }
    }
}

/// One container's contents, holding stacks as some format stored them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredContainerOf<S> {
    slots: Vec<Option<S>>,
}

/// One container's contents, as this build stores them.
type StoredContainer = StoredContainerOf<StoredStack>;

/// Why a container could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContainerError {
    /// The blob claims a version this build has no step for.
    #[error("container data is version {version}; this build writes {CONTAINER_FORMAT_VERSION}")]
    UnknownVersion {
        /// What the row said.
        version: u8,
    },

    /// The bytes did not decode as that version.
    #[error("container data version {version} did not decode")]
    Decode {
        /// What the row said.
        version: u8,
    },
}

/// Encodes a container, and reports how many stacks it could not name.
///
/// A stack whose material this world has no id for is dropped rather than
/// failing the whole container: the alternative is a chest that cannot be saved
/// at all because one thing in it came from a mod somebody removed.
#[must_use]
pub fn encode(view: &View, materials: &MaterialMap) -> (Vec<u8>, usize) {
    let mut dropped = 0;
    let stored = StoredContainer {
        slots: view
            .slots
            .iter()
            .map(|slot| {
                let stack = slot.as_ref()?;
                let Ok(material) = materials.to_world(stack.material) else {
                    dropped += 1;
                    return None;
                };
                // Every cell named, or the stack is not written: see
                // `persist::cutcells`.
                let cells = match &stack.cells {
                    Some(cells) => {
                        let Some(world) = cutcells::to_world(cells, materials) else {
                            dropped += 1;
                            return None;
                        };
                        Some(world)
                    }
                    None => None,
                };
                Some(StoredStack {
                    material,
                    units: stack.units,
                    shape: stack.shape.map(Shape::occupancy),
                    detail: stack.detail.clone(),
                    cells,
                })
            })
            .collect(),
    };
    // Never fails for a type this crate defines; an empty container on a write
    // error would be worse than one that is not written at all.
    (postcard::to_allocvec(&stored).unwrap_or_default(), dropped)
}

/// Decodes a container into a view of `slots` places, in runtime ids.
///
/// The size is the mod's, not the blob's: a mod that made its chests bigger
/// should find its old ones grown rather than refused, and one that made them
/// smaller gets what still fits — with the rest reported as dropped rather
/// than silently gone.
///
/// # Errors
///
/// [`ContainerError`] for a version with no step, or bytes that do not decode.
pub fn decode(
    version: u8,
    bytes: &[u8],
    name: &str,
    slots: usize,
    materials: &MaterialMap,
) -> Result<(View, usize), ContainerError> {
    // **Migrated, not refused**, as a player's inventory is: postcard is not
    // self-describing, so a v1 row read as v2 runs out of bytes, and refusing
    // it would empty every chest filled before the upgrade.
    let stored: StoredContainer = match version {
        CONTAINER_FORMAT_VERSION => {
            postcard::from_bytes(bytes).map_err(|_| ContainerError::Decode { version })?
        }
        1 => {
            let old: StoredContainerOf<StoredStackV1> =
                postcard::from_bytes(bytes).map_err(|_| ContainerError::Decode { version })?;
            StoredContainerOf {
                slots: old
                    .slots
                    .into_iter()
                    .map(|slot| slot.map(Into::into))
                    .collect(),
            }
        }
        _ => return Err(ContainerError::UnknownVersion { version }),
    };

    let mut dropped = 0;
    let mut view = View::empty(name, slots);
    for (index, slot) in stored.slots.iter().enumerate() {
        let Some(stack) = slot else { continue };
        let Some(built) = cutcells::thaw(
            stack.material,
            stack.units,
            stack.shape,
            stack.cells.as_deref(),
            stack.detail.clone(),
            materials,
        ) else {
            dropped += 1;
            continue;
        };
        match view.slots.get_mut(index) {
            Some(place) => *place = Some(built),
            // The container shrank. Counted rather than dropped in silence, so
            // an operator can be told that a mod's change cost somebody a row
            // of their chest.
            None => dropped += 1,
        }
    }
    Ok((view, dropped))
}

/// The empty container a name refers to before anything is put in it.
#[must_use]
pub fn empty(name: &str, slots: usize) -> View {
    View::empty(name, slots)
}

/// Whether a material id is one this world can name, for a caller checking
/// before it writes.
#[must_use]
pub fn nameable(material: MaterialId, materials: &MaterialMap) -> bool {
    materials.to_world(material).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory::Stack;

    fn shifted() -> MaterialMap {
        // Deliberately not the identity, so a test cannot pass by ignoring the
        // translation — the same fixture the player codec uses.
        MaterialMap::from_pairs(&[(MaterialId(1), 7), (MaterialId(2), 4)])
    }

    #[test]
    fn a_container_survives_the_trip_with_its_cuts_and_details() {
        let mut view = View::empty("core_chest:at:1,2,3", 4);
        view.slots[0] = Stack::new(MaterialId(1), 30);
        view.slots[2] = Stack::new(MaterialId(2), 5).map(|stack| Stack {
            shape: Shape::new(0b101),
            detail: Some("wear=3".to_owned()),
            ..stack
        });

        let (bytes, dropped) = encode(&view, &shifted());
        assert_eq!(dropped, 0);
        let (back, lost) = decode(
            CONTAINER_FORMAT_VERSION,
            &bytes,
            "core_chest:at:1,2,3",
            4,
            &shifted(),
        )
        .expect("decode");
        assert_eq!(lost, 0);
        assert_eq!(back, view, "a container came back as something else");
    }

    #[test]
    fn a_material_this_world_cannot_name_costs_one_stack_and_not_the_chest() {
        // A mod removed since the chest was filled. Refusing the whole
        // container would lose everything else in it, which is the opposite of
        // charter rule 8's round trip.
        let mut view = View::empty("core_chest:at:1,2,3", 3);
        view.slots[0] = Stack::new(MaterialId(1), 10);
        view.slots[1] = Stack::new(MaterialId(99), 10);

        let (bytes, dropped) = encode(&view, &shifted());
        assert_eq!(dropped, 1, "the unnameable stack should be the only loss");
        let (back, _) = decode(
            CONTAINER_FORMAT_VERSION,
            &bytes,
            "core_chest:at:1,2,3",
            3,
            &shifted(),
        )
        .expect("decode");
        assert_eq!(
            back.slots[0].as_ref().map(|stack| stack.units),
            Some(10),
            "the stack beside it was lost too"
        );
    }

    #[test]
    fn a_container_that_shrank_reports_what_did_not_fit() {
        let mut view = View::empty("core_chest:at:1,2,3", 4);
        view.slots[3] = Stack::new(MaterialId(1), 10);
        let (bytes, _) = encode(&view, &shifted());

        let (back, lost) = decode(
            CONTAINER_FORMAT_VERSION,
            &bytes,
            "core_chest:at:1,2,3",
            2,
            &shifted(),
        )
        .expect("decode");
        assert_eq!(back.slots.len(), 2, "the mod's size wins, not the blob's");
        assert_eq!(lost, 1, "a row that no longer fits must be reported");
    }

    #[test]
    fn a_cut_of_several_materials_survives_the_trip_in_world_ids() {
        // Sub-Node Contract §9.1: every cell's material to disk under the
        // world's id, and back under this session's.
        let mut cells = crate::block::EMPTY_CELLS;
        cells[0] = MaterialId(2);
        cells[26] = MaterialId(1);
        let mut view = View::empty("core_chest:at:1,2,3", 2);
        view.slots[1] = crate::inventory::Stack::mixed(&cells, 4);
        let (bytes, dropped) = encode(&view, &shifted());
        assert_eq!(dropped, 0);
        let (back, lost) = decode(
            CONTAINER_FORMAT_VERSION,
            &bytes,
            "core_chest:at:1,2,3",
            2,
            &shifted(),
        )
        .expect("decode");
        assert_eq!(lost, 0);
        assert_eq!(back, view);
        let stored: StoredContainer = postcard::from_bytes(&bytes).expect("v2");
        let world = stored.slots[1]
            .as_ref()
            .and_then(|stack| stack.cells.clone())
            .expect("cells on disk");
        assert_eq!(
            (world[0], world[26]),
            (4, 7),
            "the cells are not in world ids"
        );
    }

    #[test]
    fn a_container_written_before_cuts_of_several_materials_still_loads() {
        let old = StoredContainerOf {
            slots: vec![
                Some(StoredStackV1 {
                    material: 7,
                    units: 10,
                    shape: Some(0b111),
                    detail: Some("wear=1".to_owned()),
                }),
                None,
            ],
        };
        let bytes = postcard::to_allocvec(&old).expect("encode v1");
        assert!(
            postcard::from_bytes::<StoredContainer>(&bytes).is_err(),
            "a v1 row decoded as v2, so the step is not needed"
        );
        let (back, lost) = decode(1, &bytes, "c", 2, &shifted()).expect("decode v1");
        assert_eq!(lost, 0);
        let stack = back.slots[0].as_ref().expect("the v1 stack");
        assert_eq!(stack.material, MaterialId(1));
        assert_eq!(stack.shape.map(Shape::occupancy), Some(0b111));
        assert_eq!(stack.detail.as_deref(), Some("wear=1"));
        assert_eq!(stack.cells, None);
    }

    #[test]
    fn a_version_with_no_step_is_refused_rather_than_guessed_at() {
        let (bytes, _) = encode(&View::empty("x", 1), &shifted());
        assert_eq!(
            decode(99, &bytes, "x", 1, &shifted()),
            Err(ContainerError::UnknownVersion { version: 99 })
        );
    }
}
