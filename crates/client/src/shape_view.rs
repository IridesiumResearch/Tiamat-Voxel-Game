// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Drawing and chiselling a block's twenty-seven cells, in two dimensions.
//!
//! # Why there is no 3D here at all
//!
//! A shape editor wants to look like a block seen from a corner, and the
//! obvious way to get that is a camera, a depth buffer and a render pass whose
//! result is fed back into the interface. That is a lot of machinery for a
//! picture of a cube, and every bit of it would be a second path for something
//! the world renderer already does.
//!
//! An isometric projection needs neither. The one this module uses is
//! orthographic along `(1, 1, 1)`:
//!
//! ```text
//! sx = (x - z)
//! sy = (x + z) / 2 - y
//! ```
//!
//! which is exactly the projection whose depth ordering is `x + y + z` — a
//! displacement of `(1, 1, 1)` maps to `(0, 0)` on screen, so cells sort by
//! that sum and the painter's algorithm is correct rather than approximately
//! correct. Twenty-seven cells drawn back to front is cheaper than a render
//! pass and, unlike one, every function here is testable without a GPU.
//!
//! # The gesture is the world's gesture
//!
//! Left-click removes the nearest cell along the line of sight; right-click
//! puts one back against the face that was clicked. That is what digging and
//! placing already do, so a player knows it before they open the screen — and
//! it means removal never has to reach a cell it cannot see, because chiselling
//! only ever works from the outside in.

/// Cells along one edge of a block.
const SIDE: i32 = 3;

/// The projected grid is six units across, whatever the widget's size.
///
/// `x - z` runs `-3..=3` and `(x + z) / 2 - y` runs `-3..=3`, so the whole
/// block is a six-by-six square and one scale fits both axes.
const EXTENT: f32 = 6.0;

/// Which face of a cell a click landed on.
///
/// Only three can ever be visible from the fixed viewpoint, which is the point:
/// a cell the player can see is a cell they can reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Face {
    /// The `+y` face, looking down onto it.
    Top,
    /// The `+x` face, on the right.
    Right,
    /// The `+z` face, toward the viewer.
    Front,
}

impl Face {
    /// The neighbouring cell in this direction, if it is inside the block.
    #[must_use]
    pub const fn beyond(self, x: i32, y: i32, z: i32) -> Option<(i32, i32, i32)> {
        let (x, y, z) = match self {
            Self::Top => (x, y + 1, z),
            Self::Right => (x + 1, y, z),
            Self::Front => (x, y, z + 1),
        };
        if x < SIDE && y < SIDE && z < SIDE {
            Some((x, y, z))
        } else {
            None
        }
    }
}

/// How many quarter turns the cube has been orbited by.
///
/// **Quarter turns, not a free orbit**, and the reason is that everything else
/// in this module is exact. The projection's depth order is `x + y + z`
/// *exactly*, three faces are visible and always the same three, and a click
/// lands on a cell rather than nearly on one. A turn is a permutation of the
/// twenty-seven cells, so all of that survives it unchanged — where an
/// arbitrary angle would make each of those approximately true and would need
/// its own proof.
///
/// The cut a player is editing is always held in AUTHORED coordinates. This is
/// a way of looking at it and nothing more, which is why nothing outside this
/// module and the widget that draws it has to know a turn exists.
pub type Turn = u32;

/// The mask as it appears after `turn` quarter turns.
#[must_use]
pub fn as_seen(mask: u32, turn: Turn) -> u32 {
    tiamat_core::inventory::turned(mask, turn)
}

/// Where a cell the player clicked on is in the authored block.
///
/// The inverse of the turn that [`as_seen`] applied. Without it, chiselling a
/// cube that has been turned round takes out the cell that WOULD have been
/// under the pointer had nobody turned it.
#[must_use]
pub fn authored_cell(cell: (i32, i32, i32), turn: Turn) -> (i32, i32, i32) {
    let (mut x, mut y, mut z) = cell;
    // Three forward turns are one backward turn, which avoids writing the
    // inverse permutation out and getting one of its two minus signs wrong.
    for _ in 0..(4 - turn % 4) % 4 {
        let (nx, ny, nz) = (z, y, SIDE - 1 - x);
        (x, y, z) = (nx, ny, nz);
    }
    (x, y, z)
}

/// What one of the three visible faces is a face OF, in the authored block.
///
/// Returns the outward normal in authored coordinates, so a caller can ask
/// whether the face a player is looking at is the cut's front, its top or its
/// side — which is what the arrows on the cube say.
#[must_use]
pub fn authored_normal(face: Face, turn: Turn) -> [i32; 3] {
    let mut normal = match face {
        Face::Top => [0, 1, 0],
        Face::Right => [1, 0, 0],
        Face::Front => [0, 0, 1],
    };
    // The same backward turn as `authored_cell`, and expressed the same way:
    // the FORWARD map, applied `4 - turn` times. On a direction rather than on
    // a position, so there is no re-centring and the sign moves instead.
    //
    // Writing the inverse out directly is what got this wrong the first time —
    // an inverse applied `4 - turn` times is the forward map, so the arrows
    // turned the opposite way to the cube.
    for _ in 0..(4 - turn % 4) % 4 {
        normal = [normal[2], normal[1], -normal[0]];
    }
    normal
}

/// Which of the cut's own faces a visible face is, if it is a labelled one.
///
/// **The three labels are what give a cut an orientation at all.** The engine
/// turns a placed cut so its front faces the player (`place::oriented`), and a
/// player cannot ask for that without being able to see which face the front
/// is. `None` for the back and the left, which have no arrow: they are the
/// other side of two that do.
#[must_use]
pub fn label(face: Face, turn: Turn) -> Option<Label> {
    match authored_normal(face, turn) {
        [0, 1, 0] => Some(Label::Top),
        [0, 0, 1] => Some(Label::Front),
        [1, 0, 0] => Some(Label::Side),
        _ => None,
    }
}

/// One of the cut's three labelled faces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Label {
    /// `+y`. Stays up when the cut is placed.
    Top,
    /// `+z`. Turned toward whoever places the cut.
    Front,
    /// `+x`. Named so that a player can tell a turned cube from an untouched
    /// one even when the front is round the back.
    Side,
}

impl Label {
    /// What to write on the face.
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            Self::Top => "top",
            Self::Front => "front",
            Self::Side => "side",
        }
    }
}

/// The bit a cell occupies in an occupancy mask.
///
/// `index = x + 3*y + 9*z`, the layout `core::block` documents and the one a
/// `Partial` block is stored with. There is exactly one indexing convention in
/// this engine and this is it.
#[must_use]
pub const fn bit(x: i32, y: i32, z: i32) -> u32 {
    1 << (x + SIDE * y + SIDE * SIDE * z)
}

/// The index of a cell in a cells array: the position of its [`bit`].
///
/// # Panics
///
/// Never for a cell inside the block, which is the only kind this module
/// produces: `0..3` on each axis is `0..27`.
#[must_use]
pub const fn index(x: i32, y: i32, z: i32) -> usize {
    bit(x, y, z).trailing_zeros() as usize
}

/// Whether a cell is filled.
#[must_use]
pub const fn filled(mask: u32, x: i32, y: i32, z: i32) -> bool {
    mask & bit(x, y, z) != 0
}

/// Where a grid corner lands on screen.
///
/// Coordinates are corner coordinates, `0..=3`, not cell coordinates — a cell
/// spans from its own corner to the next one along each axis.
fn project(area: egui::Rect, x: f32, y: f32, z: f32) -> egui::Pos2 {
    let scale = area.width().min(area.height()) / EXTENT;
    let centre = area.center();
    // Origin-centred first: the grid runs 0..3, so subtracting 1.5 from each
    // axis puts the block's middle at the middle of the widget.
    let (x, y, z) = (x - 1.5, y - 1.5, z - 1.5);
    egui::pos2(
        centre.x + (x - z) * scale,
        // `+y` is up on screen and down in egui, hence the sign; `(x + z) / 2`
        // is what puts the far corner at the top.
        centre.y + ((x + z) * 0.5 - y) * scale,
    )
}

/// The four screen corners of one face of one cell.
#[must_use]
pub fn face_corners(area: egui::Rect, x: i32, y: i32, z: i32, face: Face) -> [egui::Pos2; 4] {
    #[expect(
        clippy::cast_precision_loss,
        reason = "grid coordinates are 0..=3 and exact in f32"
    )]
    let (x, y, z) = (x as f32, y as f32, z as f32);
    corners_of(area, (x, y, z), 1.0, face)
}

/// The four screen corners of one face of the WHOLE block.
///
/// **Not twenty-seven cells that happen to be full.** A block drawn cell by
/// cell is eighty-one quads with visible seams where their edges meet; this is
/// three, and it is the same cube in the same projection. Loose material in a
/// slot is a block, so this is what a slot draws.
#[must_use]
pub fn block_corners(area: egui::Rect, face: Face) -> [egui::Pos2; 4] {
    corners_of(area, (0.0, 0.0, 0.0), SIDE as f32, face)
}

/// One face of a cube `size` cells across, with its low corner at `at`.
fn corners_of(area: egui::Rect, at: (f32, f32, f32), size: f32, face: Face) -> [egui::Pos2; 4] {
    let (x, y, z) = at;
    let step = size;
    match face {
        Face::Top => [
            project(area, x, y + step, z),
            project(area, x + step, y + step, z),
            project(area, x + step, y + step, z + step),
            project(area, x, y + step, z + step),
        ],
        Face::Right => [
            project(area, x + step, y, z),
            project(area, x + step, y + step, z),
            project(area, x + step, y + step, z + step),
            project(area, x + step, y, z + step),
        ],
        Face::Front => [
            project(area, x, y, z + step),
            project(area, x + step, y, z + step),
            project(area, x + step, y + step, z + step),
            project(area, x, y + step, z + step),
        ],
    }
}

/// Every filled cell, ordered so that drawing them in turn is correct.
///
/// Far to near, by `x + y + z` — the depth along this projection's view axis,
/// exactly rather than approximately, which is the whole reason the projection
/// was chosen. A stable sort, so a mask never draws two ways.
#[must_use]
pub fn draw_order(mask: u32) -> Vec<(i32, i32, i32)> {
    let mut cells: Vec<(i32, i32, i32)> = (0..SIDE)
        .flat_map(|z| (0..SIDE).flat_map(move |y| (0..SIDE).map(move |x| (x, y, z))))
        .filter(|(x, y, z)| filled(mask, *x, *y, *z))
        .collect();
    cells.sort_by_key(|(x, y, z)| x + y + z);
    cells
}

/// Which cell and face a point is over, nearest first.
///
/// `None` for a point over no filled cell — the gap left by a chiselled block,
/// or the space around it.
#[must_use]
pub fn pick(area: egui::Rect, mask: u32, point: egui::Pos2) -> Option<((i32, i32, i32), Face)> {
    // Near to far, which is the reverse of the draw order for the same reason
    // it is the draw order: the first hit going forwards is the one the player
    // can see, and testing hidden faces of cells behind it can never win.
    for (x, y, z) in draw_order(mask).into_iter().rev() {
        for face in [Face::Top, Face::Right, Face::Front] {
            if inside(&face_corners(area, x, y, z, face), point) {
                return Some(((x, y, z), face));
            }
        }
    }
    None
}

/// Whether a point is inside a convex polygon given in order.
fn inside(corners: &[egui::Pos2; 4], point: egui::Pos2) -> bool {
    let mut positive = false;
    let mut negative = false;
    for index in 0..corners.len() {
        let a = corners[index];
        let b = corners[(index + 1) % corners.len()];
        let cross = (b.x - a.x) * (point.y - a.y) - (b.y - a.y) * (point.x - a.x);
        if cross > 0.0 {
            positive = true;
        } else if cross < 0.0 {
            negative = true;
        }
        // A point on an edge has a zero cross and counts as inside, so two
        // faces that share an edge both claim it — harmless, because the
        // caller takes the first.
        if positive && negative {
            return false;
        }
    }
    true
}

/// What a left click does: take the cell out.
#[must_use]
pub const fn chisel(mask: u32, x: i32, y: i32, z: i32) -> u32 {
    mask & !bit(x, y, z)
}

/// What a right click does: put a cell back against the face that was clicked.
///
/// Returns the mask unchanged when the neighbour is outside the block, which is
/// what clicking the outer face of an edge cell means.
#[must_use]
pub fn restore(mask: u32, x: i32, y: i32, z: i32, face: Face) -> u32 {
    match face.beyond(x, y, z) {
        Some((x, y, z)) => mask | bit(x, y, z),
        None => mask,
    }
}

/// What a left click on a TURNED cube does to the authored cut.
///
/// The player clicks what they can see; the cut is stored the way it was made.
/// Every gesture on a turned cube has to come back through the turn, and this
/// is where that happens — inline at the widget it was one call in the wrong
/// coordinates away from taking out a cell on the other side of the block.
#[must_use]
pub fn chisel_seen(mask: u32, turn: Turn, cell: (i32, i32, i32)) -> u32 {
    let (x, y, z) = authored_cell(cell, turn);
    chisel(mask, x, y, z)
}

/// What a right click on a turned cube does to the authored cut.
///
/// The face that was clicked is a face of the VIEW, so the neighbour is found
/// in the view and the whole answer turned back. Turning the answer rather
/// than the neighbour is what makes it impossible for this to disagree with
/// what was drawn.
#[must_use]
pub fn restore_seen(mask: u32, turn: Turn, cell: (i32, i32, i32), face: Face) -> u32 {
    let seen = as_seen(mask, turn);
    let restored = restore(seen, cell.0, cell.1, cell.2, face);
    as_seen(restored, (4 - turn % 4) % 4)
}

/// The cell a right click fills when there is nothing left to click on.
///
/// **So the editor is never a dead end.** A player who chisels every cell away
/// has no face to place against and no way back, and a screen you can put into
/// a state you cannot leave is a bug however rare it is. The middle cell,
/// because it is the one every other cell can be reached from.
#[must_use]
pub const fn seed() -> u32 {
    bit(1, 1, 1)
}

// # A cut of several materials
//
// An editor given 27 cells (Sub-Node Contract §9.1) keeps each cell's own
// material. Everything about WHERE a click lands — the turn, the face, the
// neighbour — is the mask functions above and is not written again here: each
// cells function asks its mask function which cells the gesture leaves filled
// and only says what those cells are made of. Two copies of the geometry would
// be two chances for a click to take out one cell and repaint another.

/// [`as_seen`] for cells: every cell's material moves with the cell.
///
/// Core's [`tiamat_core::inventory::turned_cells`], which shares its
/// permutation with the [`tiamat_core::inventory::turned`] that `as_seen`
/// is, so the cube drawn and the cube clicked cannot be turned two ways.
#[must_use]
pub fn cells_seen(cells: &crate::cut::Cells, turn: Turn) -> crate::cut::Cells {
    tiamat_core::inventory::turned_cells(&crate::cut::as_materials(cells), turn)
        .map(|material| material.0)
}

/// What a left click on a turned cube does to a cut of several materials: the
/// cell that was clicked goes, whatever it is made of, and nothing else moves.
#[must_use]
pub fn chisel_seen_cells(
    cells: &crate::cut::Cells,
    turn: Turn,
    cell: (i32, i32, i32),
) -> crate::cut::Cells {
    let filled = chisel_seen(crate::cut::occupancy(cells), turn, cell);
    repainted(cells, filled, 0)
}

/// What a right click on a turned cube does to a cut of several materials: a
/// cell of the BRUSH against the face that was clicked.
///
/// A cell that is already filled is never repainted — a right click adds, as
/// placing does in the world, and changing what a cell is made of is taking it
/// out and putting another back.
#[must_use]
pub fn restore_seen_cells(
    cells: &crate::cut::Cells,
    turn: Turn,
    cell: (i32, i32, i32),
    face: Face,
    brush: u16,
) -> crate::cut::Cells {
    let filled = restore_seen(crate::cut::occupancy(cells), turn, cell, face);
    repainted(cells, filled, brush)
}

/// [`seed`] for a cut of several materials: the middle cell, of the brush.
#[must_use]
pub fn seed_cells(brush: u16) -> crate::cut::Cells {
    repainted(&[0; 27], seed(), brush)
}

/// The cells once the ones they fill have become `filled`: a cell that left is
/// empty, a cell that arrived is `brush`, and every other keeps its material.
fn repainted(cells: &crate::cut::Cells, filled: u32, brush: u16) -> crate::cut::Cells {
    let mut out = *cells;
    for (index, material) in out.iter_mut().enumerate() {
        if filled & (1 << index) == 0 {
            *material = 0;
        } else if *material == 0 {
            *material = brush;
        }
    }
    out
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_chisel_on_a_turned_cube_takes_the_cell_that_was_clicked() {
        // **The failure this rules out is silent and total**: a click that
        // lands on the cell the pointer is over and removes one somewhere else
        // in the block. Every turn other than none would do it, and the shape
        // still looks like a shape afterwards.
        let full = 0x7FF_FFFF;
        for turn in 0..4 {
            let seen_cell = (2, 1, 0);
            let after = chisel_seen(full, turn, seen_cell);
            let seen = as_seen(after, turn);
            assert!(
                !filled(seen, seen_cell.0, seen_cell.1, seen_cell.2),
                "at turn {turn} the cell that was clicked is still there"
            );
            assert_eq!(
                after.count_ones(),
                26,
                "at turn {turn} a chisel took more or less than one cell"
            );
        }
    }

    #[test]
    fn a_restore_on_a_turned_cube_puts_the_cell_where_it_was_asked_for() {
        for turn in 0..4 {
            let one = bit(1, 1, 1);
            let after = restore_seen(one, turn, (1, 1, 1), Face::Top);
            assert_eq!(
                after.count_ones(),
                2,
                "at turn {turn} a restore added other than one cell"
            );
            // The cell that appeared is the one ON TOP of the middle, whatever
            // way round the cube is: the top face does not move.
            assert!(
                filled(after, 1, 2, 1),
                "at turn {turn} the new cell landed somewhere other than on top"
            );
        }
    }

    #[test]
    fn turning_the_cube_never_changes_the_cut() {
        // A view is a view. If turning could change the mask, a player would
        // craft a different shape depending on which way they happened to be
        // looking when they finished.
        let cut = 0b1_0011_0101_1100_0011;
        for turn in 0..4 {
            assert_eq!(
                as_seen(as_seen(cut, turn), (4 - turn % 4) % 4),
                cut,
                "turn {turn} did not come back"
            );
        }
    }

    #[test]
    fn turning_the_cube_and_turning_it_back_is_no_turn_at_all() {
        for turn in 0..4 {
            for cell in [(0, 0, 0), (2, 1, 0), (1, 1, 1), (2, 2, 2), (0, 2, 1)] {
                let seen = {
                    // Where the cell ends up when the cube is turned: the same
                    // permutation `as_seen` applies to the mask.
                    let mask = bit(cell.0, cell.1, cell.2);
                    let turned = as_seen(mask, turn);
                    (0..SIDE)
                        .flat_map(|z| {
                            (0..SIDE).flat_map(move |y| (0..SIDE).map(move |x| (x, y, z)))
                        })
                        .find(|(x, y, z)| filled(turned, *x, *y, *z))
                        .expect("a turn must not lose the cell")
                };
                assert_eq!(
                    authored_cell(seen, turn),
                    cell,
                    "a click on a cube turned {turn} times found the wrong cell"
                );
            }
        }
    }

    #[test]
    fn the_front_arrow_follows_the_cube_round() {
        // Untouched, the front faces the viewer and the side is on the right.
        assert_eq!(label(Face::Front, 0), Some(Label::Front));
        assert_eq!(label(Face::Right, 0), Some(Label::Side));
        assert_eq!(label(Face::Top, 0), Some(Label::Top));

        // One turn brings the front round to the right-hand face, and what was
        // the left side comes into view — which has no label, because it is the
        // back of the one that does.
        assert_eq!(label(Face::Right, 1), Some(Label::Front));
        assert_eq!(label(Face::Front, 1), None);

        // Two turns and the front is round the back: neither visible vertical
        // face is labelled, and the top still is.
        assert_eq!(label(Face::Front, 2), None);
        assert_eq!(label(Face::Right, 2), None);
        assert_eq!(label(Face::Top, 2), Some(Label::Top));

        // Three, and the front is on the face toward the viewer again? No —
        // it is the SIDE that comes round to the front.
        assert_eq!(label(Face::Front, 3), Some(Label::Side));
    }

    #[test]
    fn the_top_is_the_top_from_every_side() {
        // Turning about the vertical axis cannot move the top face, and a
        // label that drifted would tell a player their cut was upside down.
        for turn in 0..4 {
            assert_eq!(label(Face::Top, turn), Some(Label::Top));
            assert_eq!(authored_normal(Face::Top, turn), [0, 1, 0]);
        }
    }
    use super::*;

    const FULL: u32 = (1 << 27) - 1;

    fn area() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(240.0, 240.0))
    }

    #[test]
    fn the_projection_is_orthographic_along_the_view_axis() {
        // The property the depth sort rests on: a step of (1, 1, 1) is a step
        // straight away from the viewer and must not move on screen. If this
        // ever stops holding, `draw_order` is sorting by the wrong number and
        // cells will draw through each other.
        let near = project(area(), 0.0, 0.0, 0.0);
        let far = project(area(), 1.0, 1.0, 1.0);
        assert!(
            (near.x - far.x).abs() < 1e-3 && (near.y - far.y).abs() < 1e-3,
            "a step along the view axis moved on screen: {near:?} -> {far:?}"
        );
    }

    #[test]
    fn the_whole_block_fits_inside_the_widget() {
        let area = area();
        for (x, y, z) in [(0.0, 0.0, 0.0), (3.0, 3.0, 3.0), (3.0, 0.0, 0.0)] {
            let at = project(area, x, y, z);
            assert!(
                area.contains(at),
                "corner ({x}, {y}, {z}) landed outside the widget at {at:?}"
            );
        }
    }

    #[test]
    fn clicking_a_full_block_reaches_the_cell_nearest_the_viewer() {
        // The corner facing the camera is (2, 2, 2), and its three faces are
        // the ones drawn last. A click in the middle of the widget lands on the
        // block's near corner because that is what is in front.
        let area = area();
        let ((x, y, z), _) =
            pick(area, FULL, area.center()).expect("a full block fills the middle");
        assert_eq!(
            (x + y + z),
            6,
            "the click reached ({x}, {y}, {z}), which is not the nearest corner"
        );
    }

    #[test]
    fn chiselling_the_near_corner_exposes_what_was_behind_it() {
        // **The reason left-click needs no hidden-cell rule.** Removing what
        // you can see is always enough, because removing it reveals the next
        // one along the same line.
        let area = area();
        let (first, _) = pick(area, FULL, area.center()).expect("a full block");
        let mask = chisel(FULL, first.0, first.1, first.2);
        let (second, _) = pick(area, mask, area.center()).expect("something behind it");
        assert_ne!(first, second, "the same cell was picked twice");
        assert!(
            second.0 + second.1 + second.2 < first.0 + first.1 + first.2,
            "the cell revealed was not further away: {first:?} then {second:?}"
        );
    }

    #[test]
    fn a_click_off_the_block_hits_nothing() {
        let area = area();
        assert!(
            pick(area, FULL, area.left_top()).is_none(),
            "the corner of a square widget is outside a block drawn as a hexagon"
        );
        assert!(
            pick(area, 0, area.center()).is_none(),
            "an empty mask has nothing to click on"
        );
    }

    #[test]
    fn right_clicking_a_face_puts_a_cell_against_it() {
        // One cell, alone, so which face is picked is unambiguous.
        let mask = bit(1, 1, 1);
        assert_eq!(
            restore(mask, 1, 1, 1, Face::Top),
            mask | bit(1, 2, 1),
            "the cell went somewhere other than on top"
        );
        assert_eq!(restore(mask, 1, 1, 1, Face::Right), mask | bit(2, 1, 1));
        assert_eq!(restore(mask, 1, 1, 1, Face::Front), mask | bit(1, 1, 2));
    }

    #[test]
    fn a_face_on_the_outside_of_the_block_has_nothing_beyond_it() {
        let mask = bit(2, 2, 2);
        assert_eq!(
            restore(mask, 2, 2, 2, Face::Top),
            mask,
            "a cell was placed outside the block"
        );
        assert!(Face::Right.beyond(2, 1, 1).is_none());
    }

    #[test]
    fn every_cell_of_a_full_block_is_reachable_by_chiselling() {
        // **The claim that makes an outside-in tool sufficient.** Clicking the
        // middle of the widget repeatedly must empty the block eventually
        // rather than stalling on a cell nothing can reach — and the same has
        // to hold from every point over it, not just the centre.
        let area = area();
        let mut mask = FULL;
        let mut removed = 0;
        // Sweep the widget rather than hammering one point: one line of sight
        // only ever reaches three cells.
        for step in 0..40 {
            #[expect(clippy::cast_precision_loss, reason = "a small loop counter")]
            let t = step as f32 / 40.0;
            let point = area.left_top() + (area.right_bottom() - area.left_top()) * t;
            while let Some(((x, y, z), _)) = pick(area, mask, point) {
                mask = chisel(mask, x, y, z);
                removed += 1;
            }
        }
        assert!(
            removed > 0 && mask != FULL,
            "chiselling removed nothing at all"
        );
        assert_eq!(
            mask.count_ones() + removed,
            FULL.count_ones(),
            "the count of removed cells does not match what left the mask"
        );
    }

    /// Stone along the bottom-front row and oak in the middle, as world ids.
    fn stone_and_oak() -> crate::cut::Cells {
        let mut cells = [0; 27];
        for cell in cells.iter_mut().take(3) {
            *cell = 5;
        }
        cells[13] = 2;
        cells
    }

    #[test]
    fn a_turned_cube_of_several_materials_is_the_turned_mask_in_its_materials() {
        // **The drawn cube and the clicked cube are the same cube.** The
        // editor picks a cell from `as_seen(mask)` and paints from
        // `cells_seen(cells)`; if those ever turned differently, a click would
        // land on one cell while the player looked at another's material.
        let cells = stone_and_oak();
        for turn in 0..4 {
            let seen = cells_seen(&cells, turn);
            assert_eq!(
                crate::cut::occupancy(&seen),
                as_seen(crate::cut::occupancy(&cells), turn),
                "turn {turn} moved the materials somewhere their mask did not go"
            );
            assert_eq!(
                cells_seen(&seen, (4 - turn % 4) % 4),
                cells,
                "turn {turn} did not come back"
            );
        }
    }

    #[test]
    fn a_chisel_takes_the_clicked_cell_whatever_it_is_made_of() {
        let cells = stone_and_oak();
        for turn in 0..4 {
            // The oak cell sits at the middle, which no turn moves.
            let after = chisel_seen_cells(&cells, turn, (1, 1, 1));
            assert_eq!(after[13], 0, "at turn {turn} the oak cell is still there");
            let mut expected = cells;
            expected[13] = 0;
            assert_eq!(after, expected, "at turn {turn} something else changed");

            // And a stone one, clicked where the turn has put it.
            let seen = cells_seen(&cells, turn);
            let index = seen
                .iter()
                .position(|material| *material == 5)
                .expect("the stone is somewhere in every turn");
            let index = i32::try_from(index).expect("a cell index");
            let clicked = (index % 3, index / 3 % 3, index / 9);
            let after = chisel_seen_cells(&cells, turn, clicked);
            let after_seen = cells_seen(&after, turn);
            assert_eq!(
                after_seen[subnode(clicked.0, clicked.1, clicked.2)],
                0,
                "at turn {turn} the stone cell that was clicked is still there"
            );
            assert_eq!(
                after.iter().filter(|material| **material == 5).count(),
                2,
                "at turn {turn} a stone chisel took other than one stone cell"
            );
            assert_eq!(after[13], 2, "at turn {turn} a stone chisel took the oak");
        }
    }

    #[test]
    fn a_restore_adds_the_brush_and_never_repaints_a_filled_cell() {
        let cells = stone_and_oak();
        for turn in 0..4 {
            // On top of the oak in the middle, in a third material.
            let after = restore_seen_cells(&cells, turn, (1, 1, 1), Face::Top, 9);
            assert_eq!(
                after[subnode(1, 2, 1)],
                9,
                "at turn {turn} the new cell is not the brush's, on top"
            );
            assert_eq!(after[13], 2, "at turn {turn} the oak was repainted");
            for index in [0, 1, 2] {
                assert_eq!(after[index], 5, "at turn {turn} the stone was repainted");
            }
        }
        // A face whose neighbour is already filled adds nothing and paints
        // nothing: the neighbour of the middle's front is empty, so fill it,
        // then click the middle's front again.
        let filled = restore_seen_cells(&cells, 0, (1, 1, 1), Face::Front, 9);
        assert_eq!(
            restore_seen_cells(&filled, 0, (1, 1, 1), Face::Front, 7),
            filled,
            "a filled cell took a second brush"
        );
    }

    #[test]
    fn an_empty_cut_of_several_materials_seeds_the_middle_in_the_brush() {
        let seeded = seed_cells(4);
        assert_eq!(crate::cut::occupancy(&seeded), seed());
        assert_eq!(seeded[13], 4);
    }

    fn subnode(x: i32, y: i32, z: i32) -> usize {
        usize::try_from(x + 3 * y + 9 * z).expect("a cell index")
    }

    #[test]
    fn the_draw_order_runs_from_the_back() {
        let order = draw_order(FULL);
        assert_eq!(order.len(), 27, "a full block has twenty-seven cells");
        assert_eq!(order[0], (0, 0, 0), "the far corner draws first");
        assert_eq!(
            order[order.len() - 1],
            (2, 2, 2),
            "the near corner draws last"
        );
        for pair in order.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            assert!(
                a.0 + a.1 + a.2 <= b.0 + b.1 + b.2,
                "the order is not sorted by depth: {a:?} before {b:?}"
            );
        }
    }
}
