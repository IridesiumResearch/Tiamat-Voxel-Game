// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Drawing a material in the interface, from the texture the world uses.
//!
//! # One atlas, two consumers
//!
//! The world pass samples the atlas through the renderer's bind group. The
//! interface — inventory slots, the hotbar a mod draws with the tier-2 `Icon`
//! command — has to show the same pixels, or a slot and the wall built from it
//! disagree about what stone looks like. That is the sort of difference a
//! player notices and nobody can explain.
//!
//! So there is exactly one atlas texture. egui is handed a *view* of it
//! ([`egui_wgpu::Renderer::register_native_texture`]) and the layout to point
//! into it with ([`TileMap`]); neither is a second copy of the image.
//!
//! The view is the texture's bytes WITHOUT the sRGB decode the world samples
//! it with — see [`crate::render::Renderer::interface_atlas_view`]. egui does
//! its arithmetic on the bytes, and a decoding view drew every material as its
//! own linear value: a dirt block in the shape editor came out black.
//!
//! # Both halves can be missing, and separately
//!
//! The material table arrives from the server after the window exists, so on
//! the first frames there is no atlas at all — and a slot still has to draw
//! something. [`Icons::paint`] falls back to [`crate::dialog::material_tint`],
//! which is a hash of the material id: distinguishable, stable, and obviously
//! not a texture.

use crate::texture::TileMap;

/// The atlas, as the interface sees it.
///
/// Borrowed rather than owned because its two halves live in different places:
/// the texture id belongs to whoever owns the egui renderer, and the layout
/// arrives with the material table. Built at the call site each frame; it is
/// two words and a pointer.
#[derive(Clone, Copy, Default)]
pub struct Icons<'a> {
    texture: Option<egui::TextureId>,
    tiles: Option<&'a TileMap>,
    /// Materials that may not be placed: the items.
    ///
    /// **Because an item is not drawn like a block.** A block is a cube seen
    /// from a corner and a sword is not — it is a picture of a sword, and
    /// wrapping that picture around three faces makes three swords at three
    /// angles. Reported from the window, of exactly that.
    items: Option<&'a std::collections::BTreeSet<u16>>,
    /// What each material is called, for anything that has to say so.
    ///
    /// The engine's own name table, so a mod does not have to send names it
    /// already registered a second time to label its own inventory.
    names: Option<&'a std::collections::BTreeMap<u16, String>>,
    /// Billboard materials (Contract §8.4): a grass card is a picture.
    cards: Option<&'a std::collections::BTreeSet<u16>>,
    /// Model materials (Contract §8.6) whose icon has been built.
    models: Option<&'a ModelIcons>,
}

/// A model material's slot picture: its triangles, projected the way a
/// block's faces are, in a unit square, wearing the model's own skin.
#[derive(Debug, Clone)]
pub struct ModelIcon {
    /// Positions in `0..=1` of a unit square, `uv` on the skin, colour the
    /// face's shade; far triangles first.
    pub mesh: egui::Mesh,
}

/// Model icons by material.
pub type ModelIcons = std::collections::BTreeMap<u16, ModelIcon>;

/// `model` with every position scaled by `scale`, as the renderer scales its
/// own copy when a model arrives.
#[must_use]
pub fn scaled(model: &tiamat_core::model::Model, scale: f32) -> tiamat_core::model::Model {
    let mut model = model.clone();
    if (scale - 1.0).abs() > f32::EPSILON {
        for vertex in &mut model.vertices {
            for axis in &mut vertex.position {
                *axis *= scale;
            }
        }
    }
    model
}

/// The picture of a model, skinned with `texture`.
///
/// # The same view as a cube
///
/// Model space is cells with the origin at the block's bottom centre, as the
/// world draws a model block, so a point goes through the shape editor's
/// projection at `(x + 1.5, y, z + 1.5)` and a model three cells across sits
/// in a slot exactly where a cube does — the designer, 2026-10-08: a campfire
/// in a slot is the campfire, not a cube wearing its icon.
///
/// That projection looks down the `(1, 1, 1)` diagonal. A triangle whose
/// normal faces away from it is left out, and the rest are ordered by the
/// `x + y + z` of their corners, far first, which draws a model without a
/// depth buffer. The shade is the cube's — top full, right 0.78, front 0.6 —
/// read off the normal, so a flat-topped model matches the block beside it.
#[must_use]
pub fn model_icon(model: &tiamat_core::model::Model, texture: egui::TextureId) -> ModelIcon {
    let unit = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1.0, 1.0));
    let vertex = |index: &u32| {
        usize::try_from(*index)
            .ok()
            .and_then(|i| model.vertices.get(i))
    };
    let mut faces = Vec::new();
    for tri in model.indices.chunks_exact(3) {
        let (Some(a), Some(b), Some(c)) = (vertex(&tri[0]), vertex(&tri[1]), vertex(&tri[2]))
        else {
            continue;
        };
        let edge = |p: [f32; 3], q: [f32; 3]| [q[0] - p[0], q[1] - p[1], q[2] - p[2]];
        let (u, v) = (edge(a.position, b.position), edge(a.position, c.position));
        let normal = [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ];
        if normal[0] + normal[1] + normal[2] <= 0.0 {
            continue;
        }
        let depth: f32 = [a, b, c]
            .iter()
            .map(|corner| corner.position.iter().sum::<f32>())
            .sum();
        faces.push((depth, [a, b, c], shade_of(normal)));
    }
    faces.sort_by(|p, q| p.0.total_cmp(&q.0));
    let mut mesh = egui::Mesh::with_texture(texture);
    for (_, corners, colour) in faces {
        let Ok(base) = u32::try_from(mesh.vertices.len()) else {
            break;
        };
        for corner in corners {
            let [x, y, z] = corner.position;
            mesh.vertices.push(egui::epaint::Vertex {
                pos: crate::shape_view::project(unit, x + 1.5, y, z + 1.5),
                uv: egui::pos2(corner.uv[0], corner.uv[1]),
                color: colour,
            });
        }
        mesh.add_triangle(base, base + 1, base + 2);
    }
    ModelIcon { mesh }
}

/// A face's shade from its normal: the cube's three — top 1.0, right 0.78,
/// front 0.6 — and in between for the rest.
fn shade_of(normal: [f32; 3]) -> egui::Color32 {
    let length = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
    if length <= 0.0 {
        return egui::Color32::WHITE;
    }
    let (nx, ny) = (normal[0] / length, normal[1] / length);
    let factor = (0.6 + 0.4 * ny.max(0.0) + 0.18 * nx.max(0.0)).min(1.0);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a shade in 0.6..=1.0, scaled into a byte"
    )]
    egui::Color32::from_gray((factor * 255.0).clamp(0.0, 255.0) as u8)
}

impl<'a> Icons<'a> {
    /// The bridge, from an egui texture id and the atlas layout.
    #[must_use]
    pub const fn new(texture: Option<egui::TextureId>, tiles: Option<&'a TileMap>) -> Self {
        Self {
            texture,
            tiles,
            items: None,
            names: None,
            cards: None,
            models: None,
        }
    }

    /// Billboard materials, drawn flat like an item (Contract §8.4).
    #[must_use]
    pub const fn with_cards(mut self, cards: &'a std::collections::BTreeSet<u16>) -> Self {
        self.cards = Some(cards);
        self
    }

    /// The model icons built so far, drawn in place of a cube (Contract §8.6).
    #[must_use]
    pub const fn with_models(mut self, models: &'a ModelIcons) -> Self {
        self.models = Some(models);
        self
    }

    /// Whether a material is a billboard: a grass card.
    #[must_use]
    pub fn is_card(&self, material: u16) -> bool {
        self.cards.is_some_and(|cards| cards.contains(&material))
    }

    /// The built icon of a model material, if it is one and both its model
    /// and its skin have arrived.
    #[must_use]
    pub fn model_of(&self, material: u16) -> Option<&'a ModelIcon> {
        self.models?.get(&material)
    }

    /// The same, told which materials are items.
    ///
    /// Separate from [`Icons::new`] rather than a fourth argument everywhere: a
    /// caller with no material table has no items either, and the frames before
    /// one arrives are exactly when that is true.
    #[must_use]
    pub const fn with_items(mut self, items: &'a std::collections::BTreeSet<u16>) -> Self {
        self.items = Some(items);
        self
    }

    /// The same, told what each material is called.
    ///
    /// Separate for the same reason [`Icons::with_items`] is: the frames before
    /// the material table arrives have no names to give.
    #[must_use]
    pub const fn with_names(mut self, names: &'a std::collections::BTreeMap<u16, String>) -> Self {
        self.names = Some(names);
        self
    }

    /// What this material is called, if the table has arrived.
    ///
    /// `None` rather than a placeholder, so a caller can decide between showing
    /// an id and showing nothing at all — a tooltip reading `#7` is worse than
    /// no tooltip, and a debug line reading nothing is worse than `#7`.
    #[must_use]
    pub fn name_of(&self, material: u16) -> Option<&'a str> {
        self.names?.get(&material).map(String::as_str)
    }

    /// Whether this material is an item rather than a block.
    #[must_use]
    pub fn is_item(&self, material: u16) -> bool {
        self.items.is_some_and(|items| items.contains(&material))
    }

    /// Where a material is, if the atlas is up.
    ///
    /// Both halves are required: an id with no layout would sample tile zero
    /// for every material, and a layout with no id has nothing to sample.
    #[must_use]
    pub fn of(&self, material: u16) -> Option<(egui::TextureId, egui::Rect)> {
        let (texture, tiles) = (self.texture?, self.tiles?);
        let (u0, v0, u1, v1) = tiles.uv_of(material)?;
        Some((
            texture,
            egui::Rect::from_min_max(egui::pos2(u0, v0), egui::pos2(u1, v1)),
        ))
    }

    /// Draws what a stack looks like: a cut as its cells, loose material as its
    /// tile.
    ///
    /// # Why a cut cannot be drawn as a tile
    ///
    /// **A shape is the only thing that tells two stacks of one material
    /// apart.** They stack separately, they cost different amounts and they
    /// place differently, and an interface that drew the material's tile for
    /// both showed a player a block of stone where their stairs were. Reported
    /// from the window, of a cut that had just been made.
    ///
    /// The cells are the same projection the shape editor uses — see
    /// [`crate::shape_view`] — so a cut looks the same wherever it is shown:
    /// in the editor that made it, in a slot, and on the hotbar.
    ///
    /// # A cut of several materials
    ///
    /// `cells` is the wire's run of per-cell materials (Sub-Node Contract
    /// §9.1): empty for anything else, 27 world ids for a cut of several. With
    /// them every cell is drawn in its own material, and ALWAYS as cells — even
    /// a cut that fills the block, whose `shape` is `0` like loose material's.
    /// Drawn as one cube it would be a block of its lowest material, which is
    /// the one thing it is not.
    pub fn paint_stack(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        material: u16,
        shape: u32,
        cells: &[u16],
    ) {
        if let Some(cells) = crate::cut::cells_of(cells) {
            self.paint_mixed(painter, rect, &cells);
            return;
        }
        // **An item is a picture, not a solid.** A sword drawn as a cube is a
        // sword wrapped around three faces at three angles, which is what a
        // player sees and cannot unsee. Flat, filling the slot, the way every
        // game that has both draws them.
        // A grass card is a picture too (the designer, 2026-10-08): the
        // world draws it as a card, and a cube of it is three cards at three
        // angles.
        if self.is_item(material) || self.is_card(material) {
            self.paint_flat(painter, rect, material);
            return;
        }
        // A model block is its model, once the model and its skin are here;
        // until then the cube its texture gives, as before.
        if let Some(icon) = self.model_of(material)
            && !icon.mesh.is_empty()
        {
            Self::paint_model(painter, rect, icon);
            return;
        }
        if shape == 0 || shape == tiamat_core::inventory::Shape::ALL {
            self.paint_block(painter, rect, material);
            return;
        }
        self.paint_cells(painter, rect, material, shape);
    }

    /// Draws a material as a flat picture filling `rect`.
    ///
    /// One quad, textured if the atlas is up and tinted if it is not — the same
    /// fallback everything else here takes, so an item degrades like a block on
    /// the frames before the material table arrives.
    pub fn paint_flat(&self, painter: &egui::Painter, rect: egui::Rect, material: u16) {
        if let Some((texture, uv)) = self.of(material) {
            painter.image(texture, rect, uv, egui::Color32::WHITE);
        } else {
            painter.rect_filled(rect, 2.0, crate::dialog::material_tint(material));
        }
    }

    /// Draws a whole block as a cube seen from a corner.
    ///
    /// # Why a slot is not a flat square
    ///
    /// **A flat tile is one face of a block, and a player reads it as a
    /// sticker.** Reported from the window: the inventory and the hotbar
    /// "display as a square for the most part", and an angled block is what
    /// they should be. Three faces at three brightnesses is what says the thing
    /// in the slot is a solid object, and it is the same projection a cut is
    /// drawn in — so a block and a stair cut from it look like the same
    /// material seen the same way.
    ///
    /// Three quads rather than the twenty-seven cells of a full mask: the cube
    /// is identical and the seams between cell edges are not.
    pub fn paint_block(&self, painter: &egui::Painter, rect: egui::Rect, material: u16) {
        let area = square(rect);
        for face in [
            crate::shape_view::Face::Front,
            crate::shape_view::Face::Right,
            crate::shape_view::Face::Top,
        ] {
            let corners = crate::shape_view::block_corners(area, face);
            crate::dialog::paint_cell_face(painter, corners, *self, material, face);
        }
    }

    /// Draws a model material's picture into the square of `rect`.
    pub fn paint_model(painter: &egui::Painter, rect: egui::Rect, icon: &ModelIcon) {
        let area = square(rect);
        let mut mesh = icon.mesh.clone();
        for vertex in &mut mesh.vertices {
            vertex.pos =
                area.min + egui::vec2(vertex.pos.x * area.width(), vertex.pos.y * area.height());
        }
        painter.add(egui::Shape::mesh(mesh));
    }

    /// Draws a mask's cells, whatever the mask is.
    ///
    /// Separate from [`Icons::paint_stack`] because the shape EDITOR starts
    /// from a whole block and has to draw it as twenty-seven cells — that is
    /// the thing being chiselled. A whole block in a SLOT is loose material and
    /// draws as its tile.
    pub fn paint_cells(&self, painter: &egui::Painter, rect: egui::Rect, material: u16, mask: u32) {
        self.paint_each(painter, rect, mask, |_| material);
    }

    /// Draws a cut of several materials, every cell in its own.
    ///
    /// The same cells in the same order as [`Icons::paint_cells`], so a cut of
    /// stone and oak and a cut of stone alone differ only in which tile each
    /// face samples — the drawing cannot drift apart between the two modes.
    pub fn paint_mixed(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        cells: &crate::cut::Cells,
    ) {
        self.paint_each(painter, rect, crate::cut::occupancy(cells), |index| {
            cells[index]
        });
    }

    /// Every filled cell of `mask`, back to front, each face in the material
    /// `material_at` names for that cell's index (`x + 3*y + 9*z`).
    fn paint_each(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        mask: u32,
        material_at: impl Fn(usize) -> u16,
    ) {
        let area = square(rect);
        for (x, y, z) in crate::shape_view::draw_order(mask) {
            let material = material_at(crate::shape_view::index(x, y, z));
            for face in [
                crate::shape_view::Face::Front,
                crate::shape_view::Face::Right,
                crate::shape_view::Face::Top,
            ] {
                let corners = crate::shape_view::face_corners(area, x, y, z, face);
                crate::dialog::paint_cell_face(painter, corners, *self, material, face);
            }
        }
    }
}

/// The largest centred square inside `rect`.
///
/// The projection fits a square box, and stretching it would put the cube's
/// faces out of true with each other.
fn square(rect: egui::Rect) -> egui::Rect {
    let side = rect.width().min(rect.height());
    egui::Rect::from_center_size(rect.center(), egui::vec2(side, side))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_material_is_named_only_once_the_table_has_arrived() {
        // **A tooltip reading `#7` is worse than no tooltip**, because it looks
        // like the name. So this is an Option and not a placeholder: the caller
        // decides, and a slot chooses to say nothing during the frames before
        // the material table lands.
        use std::collections::BTreeMap;

        let mut names = BTreeMap::new();
        names.insert(3u16, "core:stone".to_owned());

        let blank = super::Icons::new(None, None);
        assert_eq!(
            blank.name_of(3),
            None,
            "with no table there is no name to give"
        );

        let table = super::Icons::new(None, None).with_names(&names);
        assert_eq!(table.name_of(3), Some("core:stone"));
        assert_eq!(
            table.name_of(4),
            None,
            "a material the table does not mention has no name either"
        );
    }
    use super::*;
    use crate::texture::Atlas;

    /// Two materials, so the atlas has a grid worth resolving.
    fn atlas() -> Atlas {
        Atlas::build(&[None, None, None, None])
    }

    #[test]
    fn a_material_maps_to_its_own_corner_of_the_atlas() {
        let tiles = atlas().tiles_only();
        let icons = Icons::new(Some(egui::TextureId::User(7)), Some(&tiles));
        let (_, first) = icons.of(0).expect("the atlas is up");
        let (_, third) = icons.of(2).expect("the atlas is up");
        assert_ne!(
            first, third,
            "two materials that share a rectangle would draw as the same block"
        );
        assert!(
            first.min.x >= 0.0 && first.max.x <= 1.0 && first.max.y <= 1.0,
            "a tile outside 0..1 samples off the atlas: {first:?}"
        );
    }

    #[test]
    fn the_tile_excludes_the_padding_that_stops_mips_bleeding() {
        let tiles = atlas().tiles_only();
        let icons = Icons::new(Some(egui::TextureId::User(1)), Some(&tiles));
        let (_, uv) = icons.of(0).expect("the atlas is up");
        assert!(
            uv.min.x > 0.0,
            "starting at the atlas edge would draw the padding, not the tile"
        );
    }

    #[test]
    fn half_a_bridge_draws_nothing() {
        let tiles = atlas().tiles_only();
        assert!(
            Icons::new(None, Some(&tiles)).of(0).is_none(),
            "a layout with no texture has nothing to sample"
        );
        assert!(
            Icons::new(Some(egui::TextureId::User(1)), None)
                .of(0)
                .is_none(),
            "a texture with no layout would show every material as tile zero"
        );
        assert!(
            Icons::default().of(0).is_none(),
            "the frames before the material table arrives have no atlas"
        );
    }

    /// Everything one call to [`Icons::paint_stack`] put on the screen.
    fn painted_stack(icons: Icons<'_>, shape: u32) -> Vec<egui::epaint::Primitive> {
        painted_cut(icons, shape, &[])
    }

    /// The same, for a stack that may carry cells.
    fn painted_cut(icons: Icons<'_>, shape: u32, cells: &[u16]) -> Vec<egui::epaint::Primitive> {
        let ctx = egui::Context::default();
        let output = ctx.run_ui(egui::RawInput::default(), |root| {
            let painter = root.ctx().layer_painter(egui::LayerId::background());
            icons.paint_stack(
                &painter,
                egui::Rect::from_min_size(egui::pos2(4.0, 4.0), egui::vec2(32.0, 32.0)),
                1,
                shape,
                cells,
            );
        });
        ctx.tessellate(output.shapes, 1.0)
            .into_iter()
            .map(|clipped| clipped.primitive)
            .collect()
    }

    #[test]
    fn a_cut_of_several_materials_draws_each_cell_from_its_own_tile() {
        // Sub-Node Contract §9.1 in a slot: stone and oak, and the slot must
        // show both — a stack drawn in its lowest material alone told a player
        // they had made a block of stone.
        let tiles = atlas().tiles_only();
        let id = egui::TextureId::User(5);
        let icons = Icons::new(Some(id), Some(&tiles));
        let mut cells = [0u16; 27];
        // The bottom layer in 2, the middle cell in 3: ten cells.
        for z in 0..3 {
            for x in 0..3 {
                cells[crate::shape_view::index(x, 0, z)] = 2;
            }
        }
        cells[13] = 3;
        let drawn = painted_cut(icons, 0, &cells);
        let meshes: Vec<&egui::Mesh> = drawn
            .iter()
            .filter_map(|primitive| match primitive {
                egui::epaint::Primitive::Mesh(mesh) if mesh.texture_id == id => Some(mesh),
                _ => None,
            })
            .collect();
        // Tessellation may merge the faces into one mesh, so count corners:
        // four per face, three faces a cell.
        let corners: usize = meshes.iter().map(|mesh| mesh.vertices.len()).sum();
        assert!(
            corners >= 10 * 3 * 4,
            "ten cells drew {corners} textured corners"
        );
        let mut sampled = std::collections::BTreeSet::new();
        for mesh in &meshes {
            for vertex in &mesh.vertices {
                for material in [2u16, 3] {
                    let (u0, v0, u1, v1) = tiles.uv_of(material).expect("the atlas is up");
                    if (u0..=u1).contains(&vertex.uv.x) && (v0..=v1).contains(&vertex.uv.y) {
                        sampled.insert(material);
                    }
                }
            }
        }
        assert_eq!(
            sampled.into_iter().collect::<Vec<_>>(),
            vec![2, 3],
            "a cut of two materials must sample both tiles"
        );
    }

    #[test]
    fn a_full_cut_of_several_materials_is_cells_and_not_a_cube() {
        // Its wire shape is `0`, the spelling of loose material, and a cube is
        // what loose material draws as. Counted in vertices, as the cube test
        // above is: three faces against twenty-seven cells of three.
        let tiles = atlas().tiles_only();
        let icons = Icons::new(Some(egui::TextureId::User(3)), Some(&tiles));
        let corners = |drawn: Vec<egui::epaint::Primitive>| {
            drawn
                .iter()
                .map(|primitive| match primitive {
                    egui::epaint::Primitive::Mesh(mesh) => mesh.vertices.len(),
                    egui::epaint::Primitive::Callback(_) => 0,
                })
                .sum::<usize>()
        };
        let mut cells = [2u16; 27];
        cells[26] = 3;
        let block = corners(painted_stack(icons, 0));
        let mixed = corners(painted_cut(icons, 0, &cells));
        assert!(
            mixed > block * 9,
            "a full cut of two materials drew {mixed} vertices and a block {block}"
        );
    }

    #[test]
    fn a_whole_block_in_a_slot_is_a_cube_and_not_a_square() {
        // **Reported from the window**: a slot drew one face of the atlas tile,
        // which reads as a sticker rather than as a solid thing. Three faces at
        // three brightnesses is what says it is a block.
        //
        // Counted in VERTICES rather than by eye: a flat tile is one quad and a
        // cube seen from a corner is three, and no arrangement of one quad
        // makes twelve corners.
        let tiles = atlas().tiles_only();
        let icons = Icons::new(Some(egui::TextureId::User(3)), Some(&tiles));
        let corners = |drawn: Vec<egui::epaint::Primitive>| {
            drawn
                .iter()
                .map(|primitive| match primitive {
                    egui::epaint::Primitive::Mesh(mesh) => mesh.vertices.len(),
                    egui::epaint::Primitive::Callback(_) => 0,
                })
                .sum::<usize>()
        };
        let block = corners(painted_stack(icons, 0));
        assert!(
            block >= 12,
            "a whole block drew {block} vertices, which is not three faces"
        );

        // And a cut is still its own cells, which is more of them again.
        let cut = corners(painted_stack(icons, 0b111 << 12));
        assert!(
            cut > block,
            "a three-cell cut drew {cut} vertices and a whole block drew {block}"
        );
    }

    #[test]
    fn an_item_is_a_flat_picture_and_a_block_is_a_cube() {
        // **Reported from the window**: the sword appeared "placed on a block,
        // three of them at different angles". It was — a whole material was
        // drawn as a cube seen from a corner, and wrapping a picture of a sword
        // round three faces makes three swords.
        //
        // Counted in vertices, as the cube test beside this one is: a flat
        // picture is one quad and a cube is three, and no arrangement of one
        // quad makes twelve corners.
        let tiles = atlas().tiles_only();
        let items: std::collections::BTreeSet<u16> = [1u16].into_iter().collect();
        let icons = Icons::new(Some(egui::TextureId::User(3)), Some(&tiles)).with_items(&items);
        let corners = |drawn: Vec<egui::epaint::Primitive>| {
            drawn
                .iter()
                .map(|primitive| match primitive {
                    egui::epaint::Primitive::Mesh(mesh) => mesh.vertices.len(),
                    egui::epaint::Primitive::Callback(_) => 0,
                })
                .sum::<usize>()
        };

        // Material 1 is in the item set, so it is a picture.
        let item = corners(painted_stack(icons, 0));
        assert!(
            item <= 4,
            "an item drew {item} vertices, which is more than one quad"
        );

        // The counter-example, and it is the whole test: the SAME call with the
        // same material, told only that it is not an item, is a cube.
        let empty = std::collections::BTreeSet::new();
        let blocks = Icons::new(Some(egui::TextureId::User(3)), Some(&tiles)).with_items(&empty);
        let block = corners(painted_stack(blocks, 0));
        assert!(
            block >= 12,
            "a block drew {block} vertices, so this test is not comparing two shapes"
        );
    }

    #[test]
    fn a_material_with_an_atlas_is_drawn_from_the_atlas() {
        let tiles = atlas().tiles_only();
        let id = egui::TextureId::User(11);
        let textured = painted_stack(Icons::new(Some(id), Some(&tiles)), 0);
        assert!(
            textured.iter().any(|primitive| matches!(
                primitive,
                egui::epaint::Primitive::Mesh(mesh) if mesh.texture_id == id
            )),
            "a slot with an atlas must sample it, not fall back to a tint"
        );

        // The counter-example, so the assertion above is visibly not vacuous:
        // with no atlas the same call draws with egui's own font texture,
        // which is what a flat rectangle uses.
        let tinted = painted_stack(Icons::default(), 0);
        assert!(
            tinted.iter().all(|primitive| !matches!(
                primitive,
                egui::epaint::Primitive::Mesh(mesh) if mesh.texture_id == id
            )),
            "there is no atlas to sample, so nothing may claim to have sampled one"
        );
        assert!(
            !tinted.is_empty(),
            "the fallback still has to draw something the player can see"
        );
    }

    #[test]
    fn an_unknown_material_still_gets_a_rectangle() {
        let tiles = atlas().tiles_only();
        let icons = Icons::new(Some(egui::TextureId::User(1)), Some(&tiles));
        assert!(
            icons.of(u16::MAX).is_some(),
            "an id past the table falls back to the placeholder tile, not to nothing"
        );
    }

    #[test]
    fn a_grass_card_is_a_picture_like_an_item() {
        // The designer, 2026-10-08: a billboard material (Contract §8.4) in a
        // slot is its card, flat, and not a cube wearing it on three faces.
        let tiles = atlas().tiles_only();
        // `painted_stack` paints material 1; the argument is the shape.
        let cards: std::collections::BTreeSet<u16> = [1u16].into_iter().collect();
        let corners = |drawn: Vec<egui::epaint::Primitive>| {
            drawn
                .iter()
                .map(|primitive| match primitive {
                    egui::epaint::Primitive::Mesh(mesh) => mesh.vertices.len(),
                    egui::epaint::Primitive::Callback(_) => 0,
                })
                .sum::<usize>()
        };
        let card = corners(painted_stack(
            Icons::new(Some(egui::TextureId::User(3)), Some(&tiles)).with_cards(&cards),
            0,
        ));
        assert!(card <= 4, "a card drew {card} vertices, more than one quad");
        let block = corners(painted_stack(
            Icons::new(Some(egui::TextureId::User(3)), Some(&tiles)),
            0,
        ));
        assert!(
            block >= 12,
            "not a card, the same material is a cube: {block}"
        );
    }

    /// A quad, as two triangles wound so its normal is `cross(b - a, c - a)`.
    fn quad(corners: [[f32; 3]; 4], indices: [u32; 6]) -> tiamat_core::model::Model {
        let vertex = |position: [f32; 3]| tiamat_core::model::Vertex {
            position,
            normal: [0.0, 1.0, 0.0],
            uv: [0.5, 0.5],
            joints: [0; 4],
            weights: [0.0; 4],
        };
        tiamat_core::model::Model {
            vertices: corners.iter().map(|corner| vertex(*corner)).collect(),
            indices: indices.to_vec(),
            ..Default::default()
        }
    }

    const LID: [[f32; 3]; 4] = [
        [-1.5, 3.0, -1.5],
        [1.5, 3.0, -1.5],
        [1.5, 3.0, 1.5],
        [-1.5, 3.0, 1.5],
    ];

    #[test]
    fn a_model_icon_keeps_the_faces_that_look_at_the_viewer_and_shades_them_like_a_cube() {
        // A lid across the top of the block wound to face up, and the same
        // lid wound to face down: the slot looks from above, so only the
        // first is drawn, at a cube's top shade, inside the unit square.
        let mut model = quad(LID, [0, 2, 1, 0, 3, 2]);
        model.indices.extend([0, 1, 2, 0, 2, 3]);
        let icon = model_icon(&model, egui::TextureId::User(21));
        assert_eq!(icon.mesh.indices.len(), 6, "two triangles face the viewer");
        assert_eq!(icon.mesh.texture_id, egui::TextureId::User(21));
        for vertex in &icon.mesh.vertices {
            assert!(
                (0.0..=1.0).contains(&vertex.pos.x) && (0.0..=1.0).contains(&vertex.pos.y),
                "a corner of the lid left the unit square: {:?}",
                vertex.pos
            );
            assert_eq!(vertex.color, egui::Color32::WHITE, "the top is lit in full");
        }

        // The front face, +z, is the cube's darkest: 0.6.
        let front = quad(
            [
                [-1.5, 0.0, 1.5],
                [1.5, 0.0, 1.5],
                [1.5, 3.0, 1.5],
                [-1.5, 3.0, 1.5],
            ],
            [0, 1, 2, 0, 2, 3],
        );
        let icon = model_icon(&front, egui::TextureId::User(21));
        assert_eq!(icon.mesh.indices.len(), 6);
        assert_eq!(icon.mesh.vertices[0].color, egui::Color32::from_gray(153));
    }

    #[test]
    fn a_model_icon_draws_the_far_faces_first() {
        // A floor at the bottom of the block and a lid at the top, both
        // facing up: the floor is farther down the view diagonal and comes
        // first, so the lid paints over it, which is what a depth buffer
        // would have decided.
        let mut model = quad(LID, [0, 2, 1, 0, 3, 2]);
        let floor = quad(
            [
                [-1.5, 0.0, -1.5],
                [1.5, 0.0, -1.5],
                [1.5, 0.0, 1.5],
                [-1.5, 0.0, 1.5],
            ],
            [0, 2, 1, 0, 3, 2],
        );
        model.vertices.extend(floor.vertices);
        model
            .indices
            .extend(floor.indices.iter().map(|index| index + 4));
        let icon = model_icon(&model, egui::TextureId::User(21));
        assert_eq!(icon.mesh.indices.len(), 12);
        // On screen, down is +y: the floor's corner sits lower than the lid's.
        assert!(
            icon.mesh.vertices[0].pos.y > icon.mesh.vertices[6].pos.y,
            "the first triangle drawn should be the floor"
        );
    }

    #[test]
    fn a_model_material_is_drawn_as_its_model_with_its_own_skin() {
        let tiles = atlas().tiles_only();
        let mut icons_by_material = ModelIcons::new();
        // `painted_stack` paints material 1; the argument is the shape.
        icons_by_material.insert(
            1,
            model_icon(&quad(LID, [0, 2, 1, 0, 3, 2]), egui::TextureId::User(21)),
        );
        let drawn = painted_stack(
            Icons::new(Some(egui::TextureId::User(3)), Some(&tiles))
                .with_models(&icons_by_material),
            0,
        );
        assert!(
            drawn.iter().any(|primitive| matches!(
                primitive,
                egui::epaint::Primitive::Mesh(mesh)
                    if mesh.texture_id == egui::TextureId::User(21) && mesh.vertices.len() == 6
            )),
            "the slot should carry the model's two triangles in the model's skin"
        );
        assert!(
            !drawn.iter().any(|primitive| matches!(
                primitive,
                egui::epaint::Primitive::Mesh(mesh) if mesh.texture_id == egui::TextureId::User(3)
            )),
            "and nothing of the cube its texture would have made"
        );
    }
}
