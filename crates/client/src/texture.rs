// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Decoding server-pushed textures, and packing them into an atlas.
//!
//! # Every byte here came from a server the player does not trust
//!
//! Charter rule 14, and this is its headline case: a client joins a stranger's
//! server and that server hands it PNGs. The decoder is the attack surface, and
//! the rules are not negotiable:
//!
//! - **Pure Rust only.** `png` is the image-rs decoder — no `unsafe`, fuzzed on
//!   OSS-Fuzz, and Chromium's PNG decoder since M139. No C bindings anywhere in
//!   this path.
//! - **Limits before decode, not after.** A PNG header can claim 65,535² pixels
//!   in 40 bytes. Checking the dimensions after decoding means allocating 17 GB
//!   to find out it was too big — the decompression bomb, and the reason
//!   [`Limits`] is applied to the reader rather than to the result.
//! - **A bad texture is a missing texture, never a crash.** Decoding runs
//!   isolated and a failure becomes the magenta checker, with the reason
//!   reported. One malformed file must not take a player out of the game.
//!
//! # Why magenta checks
//!
//! The fallback has to be unmistakable. A grey or white placeholder reads as a
//! deliberate texture and the player reports "the wall looks wrong" instead of
//! "this server has a broken texture". Magenta checks have meant "missing
//! texture" since Quake.

use std::io::Cursor;

/// Largest texture edge accepted, in pixels.
///
/// A block texture is 16² or 32², so 1024 was generous for a texture pack — and
/// too tight for the other thing that comes through this decoder. **A mod's
/// interface art is not a block texture**: a panel background or a frame is
/// sized in screen pixels, and a UI mod author hit this at 1254.
///
/// **Raising it costs no safety, because it was never the binding guard.**
/// [`MAX_DECODED_BYTES`] is: at four bytes a pixel it refuses anything past
/// about 1448², so a 2048² image is still refused — by the limit that bounds
/// the allocation rather than by the one that bounds the shape. What this edge
/// stops is a header claiming a dimension so large the multiply itself is
/// interesting, and 2048 does that as well as 1024 did.
pub const MAX_DIMENSION: u32 = 2048;

/// Largest decoded size accepted, in bytes.
///
/// Dimensions alone are not enough: 1024×1024 is fine, and a thousand of them
/// is a gigabyte. This bounds one texture; the atlas bounds the total.
pub const MAX_DECODED_BYTES: u64 = 8 * 1024 * 1024;

/// Edge length of one atlas tile, in pixels.
///
/// Every texture is scaled to this. A uniform grid makes the atlas coordinates
/// a multiply rather than a lookup, and mismatched sizes in one atlas are how
/// bleeding artefacts start.
pub const TILE: u32 = 16;

/// Padding around each tile, in pixels.
///
/// Mipmapping averages neighbouring pixels, and at the smallest mip level a
/// tile's neighbours are *other tiles* — so a block picks up the colour of
/// whatever was packed next to it. Padding each tile with a copy of its own
/// edge pixels means the average stays within the tile.
///
/// # Why 8, when 2 would hide the bleed
///
/// Because the padding also has to make [`TILE_PITCH`] a **power of two**, and
/// that is the property the mip chain actually depends on.
///
/// A mip level halves the whole atlas. If tiles sit on a 20-pixel grid, level 1
/// puts them on a 10-pixel grid, level 2 on 5, and level 3 on 2.5 — at which
/// point a texel straddles two tiles and no amount of padding helps, because
/// the tiles are no longer aligned to the grid being averaged. With a 32-pixel
/// pitch every level keeps tiles aligned to `32 >> level`, so a box filter
/// never mixes two tiles at any level, all the way down to one texel per tile.
///
/// The cost is 4x the atlas memory for a 16-pixel tile: 64 KiB for the sixteen
/// tiles a reference world uses, against a VRAM budget measured in hundreds of
/// megabytes. Not a trade worth thinking about twice.
pub const PADDING: u32 = 8;

/// Full pitch of one tile including padding.
///
/// **Must stay a power of two** — see [`PADDING`].
pub const TILE_PITCH: u32 = TILE + PADDING * 2;

/// Why a texture could not be used.
#[derive(Debug, Clone, thiserror::Error)]
pub enum TextureError {
    /// The PNG header declared something too large.
    ///
    /// Refused **before** decoding — see the module docs.
    #[error(
        "texture declares {width}x{height}, over the {MAX_DIMENSION}px limit. Refused before \
         decoding: a header claiming huge dimensions costs 40 bytes to send and gigabytes to \
         honour."
    )]
    TooLarge {
        /// Declared width.
        width: u32,
        /// Declared height.
        height: u32,
    },

    /// The decoded image would exceed [`MAX_DECODED_BYTES`].
    #[error("texture would decode to {bytes} bytes, over the {MAX_DECODED_BYTES}-byte limit")]
    TooHeavy {
        /// Declared size in bytes.
        bytes: u64,
    },

    /// The bytes are not a valid PNG.
    #[error("texture is not a decodable PNG: {reason}")]
    Malformed {
        /// What the decoder objected to.
        reason: String,
    },

    /// The decoder panicked.
    ///
    /// Should not happen — the decoder has no `unsafe` and is fuzzed — but a
    /// client must survive it if it ever does.
    #[error("the texture decoder panicked; this texture has been disabled")]
    Panicked,
}

/// A decoded texture: RGBA8, row-major, top-left origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

/// The reference saturation chain: dry ground, damp, and soaked.
///
/// Named here rather than in the example so the drift test and the generator
/// read from one list — a texture in one and not the other is a file nobody
/// notices has stopped matching.
pub const GROUND_CHAIN: &[(&str, [u8; 3])] = &[
    ("ground", [150, 118, 84]),
    ("damp", [112, 88, 62]),
    ("soaked", [78, 61, 43]),
];

impl Image {
    /// A solid colour.
    #[must_use]
    pub fn solid(width: u32, height: u32, colour: [u8; 4]) -> Self {
        Self {
            width,
            height,
            rgba: colour
                .iter()
                .copied()
                .cycle()
                .take((width as usize) * (height as usize) * 4)
                .collect(),
        }
    }

    /// The magenta-checker "this texture is missing" image.
    #[must_use]
    pub fn missing() -> Self {
        let mut rgba = Vec::with_capacity((TILE as usize) * (TILE as usize) * 4);
        for y in 0..TILE {
            for x in 0..TILE {
                // Quarter-tile checks: big enough to read at a distance.
                let dark = ((x / (TILE / 4)) + (y / (TILE / 4))).is_multiple_of(2);
                let colour: [u8; 4] = if dark {
                    [0, 0, 0, 255]
                } else {
                    [255, 0, 255, 255]
                };
                rgba.extend_from_slice(&colour);
            }
        }
        Self {
            width: TILE,
            height: TILE,
            rgba,
        }
    }

    /// The reference `core:white` texture: white with a faint border.
    ///
    /// The border is what makes a wall of white blocks readable. Without it the
    /// world is one undifferentiated mass and it is impossible to tell whether
    /// meshing is working at all — which matters for the first visible build.
    #[must_use]
    pub fn white_with_border() -> Self {
        Self::tinted_with_border([255, 255, 255])
    }

    /// The same tile in a colour, for a mod that wants more than one ground.
    ///
    /// **The border scales with the colour rather than being a fixed grey**, so
    /// a dark tile gets a dark edge instead of a light one drawn on top of it.
    /// White comes out at exactly 220 either way, which is what lets
    /// [`Image::white_with_border`] delegate here without the shipped PNG
    /// changing by a byte.
    #[must_use]
    pub fn tinted_with_border(colour: [u8; 3]) -> Self {
        /// How much of the colour the edge pixels keep. 0.86, as an integer
        /// ratio so no rounding creeps in between builds.
        const EDGE: u32 = 220;

        let mut rgba = Vec::with_capacity((TILE as usize) * (TILE as usize) * 4);
        for y in 0..TILE {
            for x in 0..TILE {
                let edge = x == 0 || y == 0 || x == TILE - 1 || y == TILE - 1;
                for channel in colour {
                    // Faint: visible as an edge, not as a drawn-on grid.
                    let value = if edge {
                        u8::try_from(u32::from(channel) * EDGE / 255).unwrap_or(channel)
                    } else {
                        channel
                    };
                    rgba.push(value);
                }
                rgba.push(255);
            }
        }
        Self {
            width: TILE,
            height: TILE,
            rgba,
        }
    }

    /// The pixel at `(x, y)`, or `None` if out of bounds.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let offset = ((y as usize) * (self.width as usize) + (x as usize)) * 4;
        self.rgba
            .get(offset..offset + 4)
            .map(|slice| [slice[0], slice[1], slice[2], slice[3]])
    }

    /// Nearest-neighbour resample to `TILE` square.
    ///
    /// Nearest rather than linear on purpose: voxel textures are pixel art, and
    /// smoothing them is what makes a 16×16 texture look like a smear.
    #[must_use]
    pub fn to_tile(&self) -> Self {
        if self.width == TILE && self.height == TILE {
            return self.clone();
        }
        let mut rgba = Vec::with_capacity((TILE as usize) * (TILE as usize) * 4);
        for y in 0..TILE {
            for x in 0..TILE {
                // Integer arithmetic: exact, and no float rounding to argue
                // about at the edges.
                let source_x = (x * self.width.max(1)) / TILE;
                let source_y = (y * self.height.max(1)) / TILE;
                let pixel = self
                    .pixel(
                        source_x.min(self.width.saturating_sub(1)),
                        source_y.min(self.height.saturating_sub(1)),
                    )
                    .unwrap_or([255, 0, 255, 255]);
                rgba.extend_from_slice(&pixel);
            }
        }
        Self {
            width: TILE,
            height: TILE,
            rgba,
        }
    }
}

/// Decodes a PNG with the limits applied **before** any allocation.
///
/// # Errors
///
/// [`TextureError`] naming which limit was hit or what the decoder objected to.
pub fn decode_png(bytes: &[u8]) -> Result<Image, TextureError> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));

    // The limits go on the DECODER, so they are enforced while reading the
    // header rather than checked against a result that has already been
    // allocated. This is the whole defence against a decompression bomb.
    decoder.set_limits(png::Limits {
        bytes: usize::try_from(MAX_DECODED_BYTES).unwrap_or(usize::MAX),
    });
    decoder.set_transformations(png::Transformations::normalize_to_color8());

    let mut reader = decoder.read_info().map_err(|err| TextureError::Malformed {
        reason: err.to_string(),
    })?;

    let info = reader.info();
    let (width, height) = (info.width, info.height);

    // Dimensions checked against the HEADER, before a single pixel is read.
    if width > MAX_DIMENSION || height > MAX_DIMENSION || width == 0 || height == 0 {
        return Err(TextureError::TooLarge { width, height });
    }
    let declared = u64::from(width) * u64::from(height) * 4;
    if declared > MAX_DECODED_BYTES {
        return Err(TextureError::TooHeavy { bytes: declared });
    }

    let mut buffer = vec![0u8; reader.output_buffer_size().unwrap_or(0)];
    let frame = reader
        .next_frame(&mut buffer)
        .map_err(|err| TextureError::Malformed {
            reason: err.to_string(),
        })?;

    let rgba = to_rgba(
        &buffer[..frame.buffer_size()],
        frame.color_type,
        width,
        height,
    )?;
    Ok(Image {
        width,
        height,
        rgba,
    })
}

/// Decodes a PNG, catching a panic rather than letting it reach the caller.
///
/// The decoder has no `unsafe` and is fuzzed, so this should never fire. It
/// exists because "should never" is not "cannot", and charter rule 14 asks for
/// panic isolation on the asset path specifically: a poisoned texture disables
/// that texture, never the client.
///
/// # Errors
///
/// As [`decode_png`], plus [`TextureError::Panicked`].
pub fn decode_png_isolated(bytes: &[u8]) -> Result<Image, TextureError> {
    // `AssertUnwindSafe` because the only state crossing the boundary is a
    // borrowed slice, which a panic cannot leave inconsistent.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decode_png(bytes)))
        .unwrap_or(Err(TextureError::Panicked))
}

/// Decodes a texture, falling back to the magenta checker.
///
/// Returns the reason alongside the fallback so the caller can surface a
/// per-server warning rather than silently rendering a broken world.
#[must_use]
pub fn decode_or_missing(bytes: &[u8]) -> (Image, Option<TextureError>) {
    match decode_png_isolated(bytes) {
        Ok(image) => (image, None),
        Err(err) => (Image::missing(), Some(err)),
    }
}

/// Converts a decoded frame to RGBA8.
fn to_rgba(
    data: &[u8],
    colour: png::ColorType,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, TextureError> {
    let pixels = (width as usize) * (height as usize);
    let mut rgba = Vec::with_capacity(pixels * 4);

    match colour {
        png::ColorType::Rgba => {
            if data.len() < pixels * 4 {
                return Err(TextureError::Malformed {
                    reason: format!("expected {} bytes of RGBA, got {}", pixels * 4, data.len()),
                });
            }
            rgba.extend_from_slice(&data[..pixels * 4]);
        }
        png::ColorType::Rgb => {
            if data.len() < pixels * 3 {
                return Err(TextureError::Malformed {
                    reason: format!("expected {} bytes of RGB, got {}", pixels * 3, data.len()),
                });
            }
            for chunk in data[..pixels * 3].chunks_exact(3) {
                rgba.extend_from_slice(&[chunk[0], chunk[1], chunk[2], 255]);
            }
        }
        png::ColorType::Grayscale => {
            if data.len() < pixels {
                return Err(TextureError::Malformed {
                    reason: format!("expected {pixels} bytes of grey, got {}", data.len()),
                });
            }
            for value in &data[..pixels] {
                rgba.extend_from_slice(&[*value, *value, *value, 255]);
            }
        }
        png::ColorType::GrayscaleAlpha => {
            if data.len() < pixels * 2 {
                return Err(TextureError::Malformed {
                    reason: format!(
                        "expected {} bytes of grey+alpha, got {}",
                        pixels * 2,
                        data.len()
                    ),
                });
            }
            for chunk in data[..pixels * 2].chunks_exact(2) {
                rgba.extend_from_slice(&[chunk[0], chunk[0], chunk[0], chunk[1]]);
            }
        }
        png::ColorType::Indexed => {
            // `normalize_to_color8` expands palettes, so reaching here means
            // the decoder did something unexpected rather than the file being
            // unusual.
            return Err(TextureError::Malformed {
                reason: "indexed colour survived normalisation".to_owned(),
            });
        }
    }

    Ok(rgba)
}

/// A texture atlas: every block texture in one image.
#[derive(Debug, Clone)]
pub struct Atlas {
    /// Tiles per row and column.
    pub grid: u32,
    /// The packed image.
    pub image: Image,
    /// Which tile each material occupies, indexed by material id.
    slots: Vec<u32>,
    /// How many slots hold a real texture rather than the chequer.
    filled: usize,
    /// Which tiles are alpha-TESTED rather than opaque or blended: foliage
    /// and sprites, the materials `cut_out` discards under 0.5. Their mips
    /// keep the artist's coverage — see [`Atlas::mips`].
    alpha_tested: Vec<bool>,
}

impl Atlas {
    /// Builds an atlas from per-material textures.
    ///
    /// `textures[i]` is the texture for material id `i`. A `None` becomes the
    /// magenta checker, so a material with no texture is visibly wrong rather
    /// than invisible.
    #[must_use]
    pub fn build(textures: &[Option<Image>]) -> Self {
        // Square grid, big enough for everything. Rounded up to a power of two
        // so the shader's coordinate maths is shifts rather than divisions.
        let count = textures.len().max(1) as u32;
        let mut grid = 1;
        while grid * grid < count {
            grid *= 2;
        }

        let side = grid * TILE_PITCH;
        let mut image = Image::solid(side, side, [0, 0, 0, 0]);
        let mut slots = Vec::with_capacity(textures.len());

        let mut filled = 0;
        for (index, texture) in textures.iter().enumerate() {
            let slot = u32::try_from(index).unwrap_or(0);
            let tile = texture.as_ref().map_or_else(Image::missing, Image::to_tile);
            filled += usize::from(texture.is_some());
            blit_padded(&mut image, &tile, slot % grid, slot / grid);
            slots.push(slot);
        }

        let alpha_tested = vec![false; textures.len()];
        Self {
            grid,
            image,
            slots,
            filled,
            alpha_tested,
        }
    }

    /// Marks one material's tile as alpha-tested: drawn through the cutout
    /// or billboard path, where a texel either passes the 0.5 test or does
    /// not exist.
    ///
    /// [`Atlas::mips`] rescales a marked tile's alpha per level so the share
    /// of texels that pass stays the share the artist drew. Unmarked tiles —
    /// opaque blocks, and glass, whose partial alpha is BLENDED and is its
    /// appearance — are left exactly as the box filter made them.
    pub fn mark_alpha_tested(&mut self, material: u16) {
        if let Some(tested) = self.alpha_tested.get_mut(usize::from(material)) {
            *tested = true;
        }
    }

    /// How many slots hold a real texture rather than the missing-texture
    /// chequer.
    ///
    /// **The number that says whether a world will draw.** A texture that never
    /// arrived and a texture that arrived and would not decode both end as the
    /// chequer, and this is the only thing that separates them from an atlas
    /// that is simply small.
    #[must_use]
    pub const fn filled(&self) -> usize {
        self.filled
    }

    /// The tile a material uses.
    #[must_use]
    pub fn slot_of(&self, material: u16) -> u32 {
        self.slots.get(material as usize).copied().unwrap_or(0)
    }

    /// The atlas edge length in pixels.
    #[must_use]
    pub const fn side(&self) -> u32 {
        self.grid * TILE_PITCH
    }

    /// How many mip levels of this atlas are safe to use.
    ///
    /// **Not the full chain.** A mip level halves the whole atlas, so once a
    /// level is smaller than the tile grid, one texel covers more than one tile
    /// and averaging them is unavoidable — no amount of padding can prevent it,
    /// because there is nowhere left to put the padding.
    ///
    /// The last safe level is the one where a tile is exactly one texel, which
    /// is `log2(TILE_PITCH)` levels down. Uploading further levels would let a
    /// distant white block pick up the colour of whatever was packed beside it,
    /// which is precisely the artefact the padding exists to prevent.
    ///
    /// Found by `no_mip_level_ever_mixes_two_tiles`, which failed at the first
    /// level past this bound with a texel that was a quarter of each of four
    /// tiles.
    #[must_use]
    pub const fn mip_levels(&self) -> u32 {
        TILE_PITCH.trailing_zeros() + 1
    }

    /// The mip levels the renderer uploads: filtered by [`mip_chain`],
    /// truncated to [`Atlas::mip_levels`] — and with every alpha-tested
    /// tile's holes given its colour before the chain is built, and its
    /// coverage put back after.
    ///
    /// **Why the holes need a colour.** A clear texel is never shown on its
    /// own — the cutout shader discards it — but the sampler averages it in:
    /// the chain below, and the bilinear filter within a level, both blend a
    /// leaf's edge with the texel beside it. Every shipped leaf stores black
    /// under alpha 0, so the blend was dark, and a canopy that read green
    /// close up went to dark speckle across the fog. A clear texel has no
    /// colour of its own, so [`dilate_tile_colour`] gives it its
    /// neighbours', and the chain is built from that.
    ///
    /// **Level 0 is still the artist's image, holes and all.** The interface
    /// draws icons from this same texture through egui, whose blend is
    /// premultiplied: a clear texel with a colour in it would ADD that colour
    /// over the slot behind, and every leaf in the hotbar would glow green
    /// through its holes. The holes' colour is needed where the sampler
    /// averages them, which is the levels below, so those take the dilated
    /// image and level 0 does not.
    ///
    /// **Why coverage needs putting back.** The cutout shader discards under
    /// a constant 0.5, and the chain averages alpha: a leaf texture that is
    /// sparse dots — most foliage is — averages below the threshold almost
    /// everywhere a level or two down, so a canopy thinned to speckle with
    /// distance and a forest against fog read as dissolving. The classic fix
    /// is applied per tile, per level: scale the level's alpha up until the
    /// share of texels passing the test is back to what level 0 has. Never
    /// down — a dense tile that kept its coverage is right already — and a
    /// texel the artist made fully clear has alpha 0, which no scale can
    /// raise, so holes stay holes.
    #[must_use]
    pub fn mips(&self) -> Vec<Image> {
        let mut source = self.image.clone();
        for (column, row) in self.alpha_tested_tiles() {
            dilate_tile_colour(&mut source, column, row);
        }
        let mut levels = vec![self.image.clone()];
        levels.extend(mip_levels_below(&source));
        levels.truncate(self.mip_levels() as usize);

        for (column, row) in self.alpha_tested_tiles() {
            let want = tile_coverage(&levels[0], column, row, 0);
            if want <= 0.0 {
                continue;
            }
            for (level, image) in levels.iter_mut().enumerate().skip(1) {
                restore_tile_coverage(image, column, row, level as u32, want);
            }
        }
        levels
    }

    /// The `(column, row)` of every tile marked alpha-tested, in material
    /// order.
    fn alpha_tested_tiles(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.alpha_tested
            .iter()
            .enumerate()
            .filter(|(_, tested)| **tested)
            .map(|(index, _)| {
                let slot = self.slots.get(index).copied().unwrap_or(0);
                (slot % self.grid, slot / self.grid)
            })
    }

    /// The UV rectangle of one tile, excluding its padding.
    ///
    /// Returned as `(u0, v0, u1, v1)` in `0.0..=1.0`.
    #[must_use]
    pub fn tile_uv(&self, slot: u32) -> (f32, f32, f32, f32) {
        self.tiles_only().uv_of_slot(slot)
    }

    /// Where each material sits, without carrying the pixels along.
    ///
    /// The interface needs to point at a tile — the inventory draws the same
    /// stone the wall is drawn from — but it has no use for the packed image,
    /// which for a large mod set is several megabytes the GPU already holds.
    /// Handing the interface a [`TileMap`] keeps one copy of the atlas.
    #[must_use]
    pub fn tiles_only(&self) -> TileMap {
        TileMap {
            grid: self.grid,
            slots: self.slots.clone(),
        }
    }
}

/// Where each material sits in the atlas: the layout, minus the image.
///
/// Produced by [`Atlas::tiles_only`] and held by whatever draws materials
/// outside the world pass. It answers the one question the interface asks —
/// "which rectangle of the atlas is this material?" — and nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TileMap {
    grid: u32,
    slots: Vec<u32>,
}

impl TileMap {
    /// The UV rectangle of one material's tile, excluding its padding.
    ///
    /// Returns `None` for an empty map — before the material table has
    /// arrived there is no atlas to point into, and a caller that gets a
    /// rectangle anyway would sample the placeholder and show every material
    /// as the same colour.
    #[must_use]
    pub fn uv_of(&self, material: u16) -> Option<(f32, f32, f32, f32)> {
        if self.slots.is_empty() {
            return None;
        }
        Some(self.uv_of_slot(self.slots.get(material as usize).copied().unwrap_or(0)))
    }

    /// The UV rectangle of one slot, excluding its padding.
    fn uv_of_slot(&self, slot: u32) -> (f32, f32, f32, f32) {
        let side = (self.grid * TILE_PITCH) as f32;
        let column = (slot % self.grid) as f32;
        let row = (slot / self.grid) as f32;
        let origin_x = column * TILE_PITCH as f32 + PADDING as f32;
        let origin_y = row * TILE_PITCH as f32 + PADDING as f32;
        (
            origin_x / side,
            origin_y / side,
            (origin_x + TILE as f32) / side,
            (origin_y + TILE as f32) / side,
        )
    }
}

/// Filtered mip levels of an image, largest first.
///
/// Level 0 is the image itself. Each level after it is a 2x2 average of the one
/// before — see [`halve`] for what "average" weighs — down to a single pixel.
///
/// Generated on the CPU rather than with a GPU blit chain. Three reasons, in
/// order: an atlas is built once per join and is tens of kilobytes, so this is
/// not on any hot path; a blit chain needs its own pipeline, bind groups, and
/// a render pass per level, all of which is code that can only be tested with a
/// GPU; and the result here is *checkable* — a test can assert that a level
/// never mixed two tiles, which is the property the whole padding scheme
/// exists to guarantee.
#[must_use]
pub fn mip_chain(image: &Image) -> Vec<Image> {
    let mut levels = vec![image.clone()];
    levels.extend(mip_levels_below(image));
    levels
}

/// Every level below `image`, largest first, down to a single pixel.
///
/// The image itself is not among them, so a caller can build a chain whose
/// level 0 is one image and whose levels below are filtered from another —
/// which is what [`Atlas::mips`] does with the holes of an alpha-tested tile.
fn mip_levels_below(image: &Image) -> Vec<Image> {
    let mut levels: Vec<Image> = Vec::new();
    loop {
        let previous = levels.last().unwrap_or(image);
        if previous.width <= 1 && previous.height <= 1 {
            break;
        }
        let next = halve(previous);
        levels.push(next);
    }
    levels
}

/// One level down: each texel the average of the 2x2 above it.
///
/// **Colour is weighted by alpha; alpha is the plain mean.** A texel's colour
/// counts for as much of it as can be seen. A box filter that gave a clear
/// texel's colour the same weight as an opaque one's pulled every sparse tile
/// toward whatever the artist left under alpha 0 — black, in every shipped
/// leaf — and a canopy that was green close up was dark at the distance where
/// the sampler had gone a few levels down. Alpha is coverage, and coverage is
/// the plain share, which is what [`restore_tile_coverage`] then measures.
///
/// Where the 2x2 has no alpha in it at all there is nothing to weight by, and
/// the plain mean of the colours is kept, so a fully clear region stays what
/// it was. For a 2x2 of one alpha the weights cancel and this is the plain
/// box filter exactly: an opaque tile's chain is unchanged to the byte.
fn halve(previous: &Image) -> Image {
    let width = (previous.width / 2).max(1);
    let height = (previous.height / 2).max(1);
    let mut rgba = Vec::with_capacity((width as usize) * (height as usize) * 4);

    for y in 0..height {
        for x in 0..width {
            // Summed wide and divided once. Summing in u8 wraps at the fourth
            // bright pixel, and the symptom is a mip level with dark speckles
            // that only appear at a distance; a colour times an alpha needs
            // more than u16 for the same reason.
            let mut weighted = [0u32; 3];
            let mut plain = [0u32; 3];
            let mut alpha = 0u32;
            let mut samples = 0u32;
            for dy in 0..2 {
                for dx in 0..2 {
                    if let Some(pixel) = previous.pixel(x * 2 + dx, y * 2 + dy) {
                        let weight = u32::from(pixel[3]);
                        for channel in 0..3 {
                            let value = u32::from(pixel[channel]);
                            weighted[channel] += value * weight;
                            plain[channel] += value;
                        }
                        alpha += weight;
                        samples += 1;
                    }
                }
            }
            let samples = samples.max(1);
            let colour = |channel: usize| -> u8 {
                weighted[channel]
                    .checked_div(alpha)
                    .unwrap_or(plain[channel] / samples) as u8
            };
            rgba.extend_from_slice(&[colour(0), colour(1), colour(2), (alpha / samples) as u8]);
        }
    }

    Image {
        width,
        height,
        rgba,
    }
}

/// The alpha-test threshold as the mip build counts it: 0.5 of 255, the same
/// constant `cut_out` in `world.wgsl` discards under.
const ALPHA_TEST: u8 = 128;

/// One tile's rectangle at one mip level: `(x, y, pitch)` in that level's
/// pixels, padding included.
///
/// The padding scheme is what makes this exact: [`TILE_PITCH`] is a power of
/// two, so at every level a tile occupies its own `TILE_PITCH >> level`
/// square and the box filter never mixed two tiles into one texel. The rect
/// takes the padding with it on purpose — the padding is a copy of the
/// tile's own edge, and rescaling the interior without it would put a seam
/// where the sampler reads across the boundary.
const fn tile_rect(column: u32, row: u32, level: u32) -> (u32, u32, u32) {
    let pitch = TILE_PITCH >> level;
    let pitch = if pitch == 0 { 1 } else { pitch };
    (column * pitch, row * pitch, pitch)
}

/// The share of one tile's texels that pass the alpha test at one level.
fn tile_coverage(image: &Image, column: u32, row: u32, level: u32) -> f32 {
    coverage_scaled(image, column, row, level, 1.0)
}

/// The share that would pass if the tile's alpha were multiplied by `scale`.
fn coverage_scaled(image: &Image, column: u32, row: u32, level: u32, scale: f32) -> f32 {
    let (x0, y0, pitch) = tile_rect(column, row, level);
    let mut passing = 0u32;
    for y in y0..y0 + pitch {
        for x in x0..x0 + pitch {
            if let Some(pixel) = image.pixel(x, y) {
                let alpha = (f32::from(pixel[3]) * scale).min(255.0);
                passing += u32::from(alpha >= f32::from(ALPHA_TEST));
            }
        }
    }
    passing as f32 / (pitch * pitch) as f32
}

/// Scales one tile's alpha up until its coverage is back to `want`.
///
/// The smallest such scale, found by bisection — coverage is monotone in the
/// scale, so eight rounds pin it well past the 1/255 the alpha can express.
/// A tile already at or over `want` is left alone, and a tile whose faded
/// alpha cannot reach it at the cap takes the cap: at the deepest level a
/// tile is one averaged texel, and a distant tree drawn solid is right where
/// a distant tree missing is a hole in the forest.
fn restore_tile_coverage(image: &mut Image, column: u32, row: u32, level: u32, want: f32) {
    const CAP: f32 = 32.0;
    if coverage_scaled(image, column, row, level, 1.0) >= want {
        return;
    }
    let mut scale = CAP;
    if coverage_scaled(image, column, row, level, CAP) >= want {
        let (mut low, mut high) = (1.0f32, CAP);
        for _ in 0..8 {
            let middle = (low + high) * 0.5;
            if coverage_scaled(image, column, row, level, middle) >= want {
                high = middle;
            } else {
                low = middle;
            }
        }
        scale = high;
    }
    let (x0, y0, pitch) = tile_rect(column, row, level);
    for y in y0..y0 + pitch {
        for x in x0..x0 + pitch {
            let index = ((y * image.width + x) as usize) * 4 + 3;
            if let Some(alpha) = image.rgba.get_mut(index) {
                *alpha = (f32::from(*alpha) * scale).min(255.0) as u8;
            }
        }
    }
}

/// Gives one tile's clear texels the colour of the texels beside them.
///
/// A texel with alpha 0 has no colour of its own — nothing ever shows it —
/// but the sampler averages it in, within a level and down the chain, so
/// what the artist left under it is what a leaf's edge blends toward. Here
/// every clear texel takes the mean colour of its nearest coloured
/// neighbours, grown outward one texel per pass until every clear texel in
/// the rect has been reached. Alpha is never touched: a hole stays a hole,
/// and [`restore_tile_coverage`] measures exactly what it did before.
///
/// Over the padded rect rather than the tile alone: the padding is a copy of
/// the tile's own edge, the sampler reads across into it, and a clear texel
/// left black there would put a dark seam where the padding meets the tile.
/// The rect is the tile's own — [`tile_rect`] — so nothing here can reach a
/// neighbouring tile.
///
/// Each pass reads the colours the previous pass left and writes its own
/// after it has looked at every texel, so the result does not depend on the
/// order the rect is walked in. A colour reaches the far corner of the rect
/// in fewer passes than the rect has texels along its two sides, which
/// bounds the loop; a real leaf's holes are a few texels wide and the fixed
/// point comes long before that.
fn dilate_tile_colour(image: &mut Image, column: u32, row: u32) {
    let rect = tile_rect(column, row, 0);
    let (x0, y0, pitch) = rect;
    let local = |x: u32, y: u32| ((y - y0) * pitch + (x - x0)) as usize;

    // Which texels have a colour to give: any alpha at all, or filled by an
    // earlier pass.
    let mut coloured = Vec::with_capacity((pitch * pitch) as usize);
    for y in y0..y0 + pitch {
        for x in x0..x0 + pitch {
            coloured.push(image.pixel(x, y).is_some_and(|pixel| pixel[3] > 0));
        }
    }

    for _ in 0..pitch * 2 {
        let mut fills = Vec::new();
        for y in y0..y0 + pitch {
            for x in x0..x0 + pitch {
                if coloured[local(x, y)] {
                    continue;
                }
                if let Some(colour) = neighbours_colour(image, &coloured, rect, x, y) {
                    fills.push((x, y, colour));
                }
            }
        }
        if fills.is_empty() {
            break;
        }
        for (x, y, colour) in fills {
            let index = ((y * image.width + x) as usize) * 4;
            if let Some(slice) = image.rgba.get_mut(index..index + 3) {
                slice.copy_from_slice(&colour);
            }
            coloured[local(x, y)] = true;
        }
    }
}

/// The mean colour of the coloured 4-neighbours of `(x, y)` inside `rect`,
/// or `None` while none of them has a colour yet.
fn neighbours_colour(
    image: &Image,
    coloured: &[bool],
    rect: (u32, u32, u32),
    x: u32,
    y: u32,
) -> Option<[u8; 3]> {
    let (x0, y0, pitch) = rect;
    let mut total = [0u32; 3];
    let mut count = 0u32;
    for (dx, dy) in [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)] {
        let (Some(nx), Some(ny)) = (x.checked_add_signed(dx), y.checked_add_signed(dy)) else {
            continue;
        };
        if nx < x0 || nx >= x0 + pitch || ny < y0 || ny >= y0 + pitch {
            continue;
        }
        let local = ((ny - y0) * pitch + (nx - x0)) as usize;
        if !coloured.get(local).copied().unwrap_or(false) {
            continue;
        }
        let Some(pixel) = image.pixel(nx, ny) else {
            continue;
        };
        for channel in 0..3 {
            total[channel] += u32::from(pixel[channel]);
        }
        count += 1;
    }
    (count > 0).then(|| {
        [
            (total[0] / count) as u8,
            (total[1] / count) as u8,
            (total[2] / count) as u8,
        ]
    })
}

/// Copies a tile into the atlas, extending its edge pixels into the padding.
///
/// The padding is a copy of the tile's own border. At the smallest mip level a
/// tile's neighbours are other tiles, so without this a white block picks up
/// the colour of whatever was packed beside it — the classic atlas bleed.
fn blit_padded(atlas: &mut Image, tile: &Image, column: u32, row: u32) {
    let base_x = column * TILE_PITCH;
    let base_y = row * TILE_PITCH;

    for y in 0..TILE_PITCH {
        for x in 0..TILE_PITCH {
            // Clamp into the tile: coordinates inside the padding read the
            // nearest real pixel, which is what extends the edge.
            let source_x = x.saturating_sub(PADDING).min(TILE - 1);
            let source_y = y.saturating_sub(PADDING).min(TILE - 1);
            let pixel = tile.pixel(source_x, source_y).unwrap_or([255, 0, 255, 255]);

            let target_x = base_x + x;
            let target_y = base_y + y;
            let offset = ((target_y as usize) * (atlas.width as usize) + (target_x as usize)) * 4;
            if let Some(slice) = atlas.rgba.get_mut(offset..offset + 4) {
                slice.copy_from_slice(&pixel);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A valid PNG of the given size, built by the encoder rather than by hand.
    fn png_bytes(width: u32, height: u32) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("header");
            let data = vec![128u8; (width as usize) * (height as usize) * 4];
            writer.write_image_data(&data).expect("data");
        }
        out
    }

    #[test]
    fn a_shipped_reference_texture_matches_the_image_it_was_generated_from() {
        // The PNG in `game/core_blocks/textures/` is written by the
        // `write_reference_textures` example from `Image::white_with_border`.
        // A checked-in binary nobody can regenerate is a file that drifts:
        // someone opens it in an editor, saves it with a different gamma, and
        // the faint border that made block edges readable is gone with no diff
        // anyone can review.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../game/core_blocks/textures/white.png");
        let bytes = std::fs::read(&path).expect("the reference mod ships this texture");

        let decoded = decode_png(&bytes).expect("and it must be a decodable PNG");
        assert_eq!(
            decoded,
            Image::white_with_border(),
            "{} has drifted from its generator; re-run \
             `cargo run -p client --example write_reference_textures -- game`",
            path.display()
        );
    }

    #[test]
    fn every_shipped_ground_texture_matches_its_generator() {
        // The same pinning `white.png` gets, for the saturation chain. A chain
        // whose three steps looked identical would make the mechanism
        // invisible, and a hand-edited PNG is how they would drift back
        // together without a diff anyone can review.
        for (name, colour) in GROUND_CHAIN {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../../game/core_blocks/textures/{name}.png"));
            let bytes = std::fs::read(&path).expect("the reference mod ships this texture");
            let decoded = decode_png(&bytes).expect("and it must be a decodable PNG");
            assert_eq!(
                decoded,
                Image::tinted_with_border(*colour),
                "{} has drifted from its generator; re-run \
                 `cargo run -p client --example write_reference_textures -- game`",
                path.display()
            );
        }
    }

    #[test]
    fn the_chain_gets_darker_at_every_step() {
        // The mechanism has to be VISIBLE or the fixture proves nothing from
        // the window. Three tiles nobody can tell apart is a saturation chain
        // that looks broken.
        let brightness =
            |colour: [u8; 3]| u32::from(colour[0]) + u32::from(colour[1]) + u32::from(colour[2]);
        for pair in GROUND_CHAIN.windows(2) {
            assert!(
                brightness(pair[1].1) < brightness(pair[0].1),
                "`{}` is not darker than `{}`",
                pair[1].0,
                pair[0].0
            );
        }
    }

    #[test]
    fn a_valid_png_decodes_to_rgba() {
        let image = decode_png(&png_bytes(16, 16)).expect("decode");
        assert_eq!((image.width, image.height), (16, 16));
        assert_eq!(image.rgba.len(), 16 * 16 * 4);
        assert_eq!(image.pixel(0, 0), Some([128, 128, 128, 128]));
    }

    #[test]
    fn an_oversized_png_is_refused_from_its_header() {
        // The decompression bomb. A header claiming huge dimensions costs a few
        // bytes to send; honouring it costs gigabytes.
        //
        // The image is COMPLETE and valid — 1025x1 is only 4 KB — so the only
        // thing that can reject it is the dimension check. An earlier version
        // wrote a header with no IDAT, which the decoder rejected as malformed
        // before the check ever ran: the test passed for the wrong reason and
        // proved nothing about the limit.
        let bytes = png_bytes(MAX_DIMENSION + 1, 1);

        let err = decode_png(&bytes).expect_err("must refuse");
        assert!(
            matches!(err, TextureError::TooLarge { width, .. } if width == MAX_DIMENSION + 1),
            "expected a dimension refusal, got {err}"
        );
        assert!(
            err.to_string().contains("before decoding"),
            "the message should say why it matters: {err}"
        );
    }

    #[test]
    fn a_texture_at_exactly_the_limit_is_accepted() {
        // The boundary, from the other side. An off-by-one here would reject a
        // legitimate high-resolution pack.
        let image = decode_png(&png_bytes(MAX_DIMENSION, 1)).expect("the limit itself is fine");
        assert_eq!(image.width, MAX_DIMENSION);
    }

    #[test]
    fn interface_art_the_size_a_ui_mod_actually_uses_decodes() {
        // **The reason the edge limit is 2048 and not 1024.** A block texture is
        // 16² and a panel background is sized in screen pixels; a UI mod author
        // hit the old cap at 1254, which is an ordinary size for a frame and
        // nowhere near large enough to be a threat.
        let image = decode_png(&png_bytes(1254, 1254)).expect("1254² is interface art");
        assert_eq!((image.width, image.height), (1254, 1254));

        // And the guard that actually bounds the allocation still does. At four
        // bytes a pixel this is past MAX_DECODED_BYTES, so it is refused for
        // its WEIGHT rather than its shape — which is the check that matters.
        let err = decode_png(&png_bytes(MAX_DIMENSION, MAX_DIMENSION))
            .expect_err("2048² is 16 MiB decoded and must not be allocated");
        assert!(
            matches!(err, TextureError::TooHeavy { .. }),
            "expected the byte limit to refuse it, got {err:?}"
        );
    }

    #[test]
    fn a_zero_dimension_png_is_refused() {
        // Zero is not "empty", it is a division waiting to happen.
        let mut out = Vec::new();
        let encoder = png::Encoder::new(&mut out, 0, 0);
        drop(encoder);
        // A hand-built header, since the encoder will not write a zero-sized
        // image.
        let mut bytes = png_bytes(1, 1);
        // Corrupt the width field in the IHDR (bytes 16..20).
        bytes[16..20].copy_from_slice(&0u32.to_be_bytes());
        assert!(decode_png(&bytes).is_err());
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        for bytes in [
            &b""[..],
            &b"not a png"[..],
            &[0x89, 0x50, 0x4E, 0x47][..], // the magic, and nothing else
            &[0xFF; 512][..],
        ] {
            let result = decode_png_isolated(bytes);
            assert!(result.is_err(), "garbage should not decode");
        }
    }

    #[test]
    fn a_truncated_png_never_panics_and_never_exceeds_its_declared_size() {
        // The property that matters is NOT "a truncated file fails" — the png
        // decoder can legitimately return a partial image, and a half-drawn
        // texture is the server's problem rather than a security one. What must
        // hold is that the client survives and the result is bounded.
        //
        // The first version of this asserted an error and failed at one cut
        // length where the decoder succeeded. Asserting the wrong property
        // would have meant either deleting a real test or "fixing" working code.
        let full = png_bytes(32, 32);
        for cut in 1..full.len() {
            if let Ok(image) = decode_png_isolated(&full[..cut]) {
                assert_eq!(
                    image.rgba.len(),
                    (image.width as usize) * (image.height as usize) * 4,
                    "a partial decode must still be internally consistent at cut {cut}"
                );
                assert!(image.width <= MAX_DIMENSION && image.height <= MAX_DIMENSION);
            }
        }
    }

    #[test]
    fn a_failed_texture_becomes_the_magenta_checker_with_a_reason() {
        // Charter rule 14: a poisoned asset disables that asset with a
        // user-visible warning, never a crash.
        let (image, error) = decode_or_missing(b"definitely not a png");

        assert!(error.is_some(), "the reason must be reported");
        assert_eq!((image.width, image.height), (TILE, TILE));
        assert!(
            image.rgba.chunks_exact(4).any(|p| p == [255, 0, 255, 255]),
            "the fallback must be visibly magenta"
        );
    }

    #[test]
    fn the_reference_white_texture_has_a_visible_border() {
        // Without it, a wall of white blocks is one undifferentiated mass and
        // there is no way to tell whether meshing works at all.
        let image = Image::white_with_border();
        let corner = image.pixel(0, 0).expect("corner");
        let middle = image.pixel(TILE / 2, TILE / 2).expect("middle");

        assert_ne!(corner, middle, "the border must differ from the face");
        assert_eq!(middle, [255, 255, 255, 255], "the face should be white");
        assert!(corner[0] < 255, "the border should be darker");
        assert!(corner[0] > 180, "but faint, not a drawn-on grid");
    }

    #[test]
    fn resampling_is_nearest_neighbour() {
        // Voxel textures are pixel art. Smoothing turns a 16x16 into a smear.
        let mut source = Image::solid(32, 32, [0, 0, 0, 255]);
        // A single white pixel in the top-left quadrant.
        source.rgba[0..4].copy_from_slice(&[255, 255, 255, 255]);

        let tile = source.to_tile();
        assert_eq!((tile.width, tile.height), (TILE, TILE));
        // Nearest neighbour keeps it pure white; a linear filter would have
        // blended it toward black.
        assert_eq!(tile.pixel(0, 0), Some([255, 255, 255, 255]));
    }

    #[test]
    fn an_atlas_packs_every_material_and_pads_each_tile() {
        let textures = vec![
            Some(Image::solid(TILE, TILE, [255, 0, 0, 255])),
            Some(Image::solid(TILE, TILE, [0, 255, 0, 255])),
            None,
        ];
        let atlas = Atlas::build(&textures);

        assert!(atlas.grid >= 2, "three tiles need at least a 2x2 grid");
        assert_eq!(atlas.image.width, atlas.side());
        assert_eq!(atlas.image.height, atlas.side());

        // The first tile's interior is its own colour.
        let (u0, v0, _, _) = atlas.tile_uv(0);
        let x = (u0 * atlas.side() as f32) as u32;
        let y = (v0 * atlas.side() as f32) as u32;
        assert_eq!(atlas.image.pixel(x, y), Some([255, 0, 0, 255]));
    }

    #[test]
    fn tile_padding_repeats_the_edge_rather_than_the_neighbour() {
        // The atlas-bleed defence. At the smallest mip a tile's neighbours are
        // other tiles, so unpadded packing paints one block with another's
        // colour.
        let textures = vec![
            Some(Image::solid(TILE, TILE, [255, 0, 0, 255])),
            Some(Image::solid(TILE, TILE, [0, 0, 255, 255])),
        ];
        let atlas = Atlas::build(&textures);

        // The pixel just left of tile 0's interior is inside tile 0's padding
        // and must be tile 0's colour, not the atlas background or tile 1's.
        let padding_pixel = atlas.image.pixel(0, PADDING).expect("in atlas");
        assert_eq!(
            padding_pixel,
            [255, 0, 0, 255],
            "padding must repeat the tile's own edge"
        );

        // And the pixel just right of tile 0's interior, still in its padding.
        let right = atlas
            .image
            .pixel(PADDING + TILE + PADDING - 1, PADDING)
            .expect("in atlas");
        assert_eq!(right, [255, 0, 0, 255], "the right padding too");
    }

    #[test]
    fn the_tile_pitch_is_a_power_of_two() {
        // The mip chain depends on it, not on the padding width. On a pitch
        // that is not a power of two, tiles stop being aligned to the grid a
        // mip level averages, and a texel straddles two of them.
        assert!(
            TILE_PITCH.is_power_of_two(),
            "TILE_PITCH is {TILE_PITCH}, which breaks the mip chain"
        );
    }

    #[test]
    fn a_raw_mip_chain_runs_all_the_way_down_to_one_pixel() {
        let atlas = Atlas::build(&[Some(Image::white_with_border())]);
        let levels = mip_chain(&atlas.image);

        assert_eq!(levels[0].width, atlas.side());
        let smallest = levels.last().expect("at least one level");
        assert_eq!((smallest.width, smallest.height), (1, 1));
        assert_eq!(levels.len(), atlas.side().trailing_zeros() as usize + 1);
    }

    #[test]
    fn the_uploaded_chain_stops_where_a_tile_is_one_texel() {
        // A single-tile atlas can use the whole chain, because a level smaller
        // than one tile does not exist. A four-tile atlas is twice as wide, so
        // its last two levels would cover more than one tile each and are
        // dropped — the count is a property of the TILE, not of the atlas.
        let one = Atlas::build(&[Some(Image::white_with_border())]);
        let four = Atlas::build(&[
            Some(Image::white_with_border()),
            Some(Image::missing()),
            Some(Image::missing()),
            Some(Image::missing()),
        ]);

        assert_eq!(one.mip_levels(), four.mip_levels());
        assert_eq!(one.mips().len() as u32, one.mip_levels());
        assert_eq!(
            four.mips().last().map(|level| level.width),
            Some(four.grid),
            "the last usable level is one texel per tile"
        );
    }

    #[test]
    fn no_mip_level_ever_mixes_two_tiles() {
        // The property the whole padding scheme exists for, checked rather than
        // asserted in a comment. Two tiles of unmistakably different colours:
        // if any level bled, a texel of one would carry some of the other.
        //
        // This is what found the bound in `mip_levels`: run against the FULL
        // chain it fails at the first level past it, with a texel that is a
        // quarter of each of four tiles.
        let red = Image::solid(TILE, TILE, [255, 0, 0, 255]);
        let blue = Image::solid(TILE, TILE, [0, 0, 255, 255]);
        let atlas = Atlas::build(&[Some(red), Some(blue)]);

        for (level, image) in atlas.mips().iter().enumerate() {
            for y in 0..image.height {
                for x in 0..image.width {
                    let pixel = image.pixel(x, y).expect("in bounds");
                    // Every texel must be pure red, pure blue, or the
                    // transparent filler between rows — never a mixture.
                    let pure = (pixel[0] == 0 || pixel[2] == 0) || pixel[3] == 0;
                    assert!(
                        pure,
                        "mip level {level} texel ({x}, {y}) is {pixel:?}: two tiles were averaged \
                         together"
                    );
                }
            }
        }
    }

    #[test]
    fn mip_averaging_does_not_wrap() {
        // Summed in u8, four bright pixels overflow and the level comes out
        // dark. The symptom is speckling that only appears at a distance.
        let white = Image::solid(4, 4, [255, 255, 255, 255]);
        let levels = mip_chain(&white);
        for level in &levels {
            assert!(
                level.pixel(0, 0) == Some([255, 255, 255, 255]),
                "a uniform white image must stay white at every mip level"
            );
        }
    }

    /// The colour every leaf-fixture dot is drawn in.
    const LEAF: [u8; 3] = [40, 90, 40];

    /// A leaf texture as the shipped ones are: sparse dots of one colour over
    /// texels that are clear AND black.
    ///
    /// Black under the holes, deliberately. Every shipped leaf PNG stores
    /// (0, 0, 0) under alpha 0, and an earlier fixture that stored the leaf
    /// colour there was exactly why the darkening it caused went unseen.
    fn leaf_tile() -> Image {
        let mut leaves = Image::solid(TILE, TILE, [0, 0, 0, 0]);
        for y in 0..TILE {
            for x in 0..TILE {
                if (x * 7 + y * 13) % 10 < 3 {
                    let at = ((y * TILE + x) as usize) * 4;
                    leaves.rgba[at..at + 4].copy_from_slice(&[LEAF[0], LEAF[1], LEAF[2], 255]);
                }
            }
        }
        leaves
    }

    /// Every texel of one tile's padded rect at one level, with its position.
    fn tile_texels(image: &Image, level: u32) -> Vec<(u32, u32, [u8; 4])> {
        let (x0, y0, pitch) = tile_rect(0, 0, level);
        let mut texels = Vec::new();
        for y in y0..y0 + pitch {
            for x in x0..x0 + pitch {
                texels.push((x, y, image.pixel(x, y).expect("in bounds")));
            }
        }
        texels
    }

    #[test]
    fn a_cutout_tiles_coverage_survives_the_whole_mip_chain() {
        // A leaf texture is sparse dots, and the chain averages alpha: left
        // alone, the deeper levels fail the 0.5 test almost everywhere and a
        // distant canopy dissolves into speckle against the fog. First
        // reported as a forest of firs gone ghostly in a blizzard.
        let mut atlas = Atlas::build(&[Some(leaf_tile())]);

        // The defect, demonstrated, so this test cannot go vacuous: unmarked,
        // the chain loses most of the dots by the third level.
        let plain = atlas.mips();
        let want = tile_coverage(&plain[0], 0, 0, 0);
        assert!(want > 0.2, "the dot pattern should cover about a third");
        let faded = tile_coverage(&plain[3], 0, 0, 3);
        assert!(
            faded < want * 0.5,
            "the chain alone kept {faded} of {want}, so this test has stopped biting"
        );

        atlas.mark_alpha_tested(0);
        for (level, image) in atlas.mips().iter().enumerate() {
            let kept = tile_coverage(image, 0, 0, level as u32);
            assert!(
                kept >= want - 1e-6,
                "level {level} kept {kept} of the artist's {want}"
            );
        }
    }

    #[test]
    fn a_mip_texel_takes_its_colour_only_from_texels_that_have_any() {
        // One opaque leaf texel among three clear black ones. Averaged with
        // equal weight it is a quarter as bright as the leaf; weighted by
        // alpha it IS the leaf, and only its alpha says how much of the 2x2
        // was leaf.
        let mut corner = Image::solid(2, 2, [0, 0, 0, 0]);
        corner.rgba[..4].copy_from_slice(&[LEAF[0], LEAF[1], LEAF[2], 255]);
        let levels = mip_chain(&corner);
        assert_eq!(levels[1].pixel(0, 0), Some([LEAF[0], LEAF[1], LEAF[2], 63]));

        // Nothing to weight by: a fully clear 2x2 keeps the plain mean of
        // whatever colours it had, so it is at least still what it was.
        let mut clear = Image::solid(2, 2, [8, 8, 8, 0]);
        clear.rgba[..4].copy_from_slice(&[40, 40, 40, 0]);
        assert_eq!(mip_chain(&clear)[1].pixel(0, 0), Some([16, 16, 16, 0]));

        // One alpha throughout — an opaque block, a pane of glass — and the
        // weights cancel: exactly the plain box filter, byte for byte.
        for alpha in [255u8, 96] {
            let mut block = Image::solid(2, 2, [0, 0, 255, alpha]);
            block.rgba[..4].copy_from_slice(&[255, 0, 0, alpha]);
            assert_eq!(
                mip_chain(&block)[1].pixel(0, 0),
                Some([63, 0, 191, alpha]),
                "a 2x2 of one alpha ({alpha}) must average as it always did"
            );
        }
    }

    #[test]
    fn a_cutout_tiles_colour_survives_the_whole_mip_chain() {
        // **The dark forest across the fog.** Every shipped leaf stores black
        // under its holes, and a box filter that averaged the holes in with
        // equal weight made every level darker than the one above — by the
        // fifth, a distant tree was one texel at a fifth of the leaf's
        // brightness, and fog only blends that near-black toward the sky.
        // Restoring the coverage made it worse to look at: the texels it
        // promoted past the test were exactly the ones the holes had
        // darkened.
        let mut atlas = Atlas::build(&[Some(leaf_tile())]);
        atlas.mark_alpha_tested(0);

        for (level, image) in atlas.mips().iter().enumerate().skip(1) {
            let mut passing = 0;
            for (x, y, texel) in tile_texels(image, level as u32) {
                if texel[3] < ALPHA_TEST {
                    continue;
                }
                passing += 1;
                assert_eq!(
                    [texel[0], texel[1], texel[2]],
                    LEAF,
                    "level {level} texel ({x}, {y}) is drawn at {texel:?}, not the leaf's colour"
                );
            }
            assert!(passing > 0, "level {level} draws nothing at all");
        }
    }

    #[test]
    fn a_cutout_tiles_holes_take_the_colour_beside_them_below_level_zero() {
        // The other half of the same defect. Weighting by alpha keeps a
        // DRAWN texel the leaf's colour, but a texel whose whole 2x2 was
        // hole is still clear and still black, and the sampler's bilinear
        // filter blends a leaf's edge with it within the level. So below
        // level 0 there must be no black texel anywhere in a marked tile's
        // rect, clear or not.
        let mut atlas = Atlas::build(&[Some(leaf_tile())]);

        // Demonstrated first: unmarked, the pattern has whole 2x2 holes, so
        // level 1 keeps clear black texels the filter will blend toward.
        let plain = atlas.mips();
        assert!(
            tile_texels(&plain[1], 1)
                .iter()
                .any(|(_, _, texel)| *texel == [0, 0, 0, 0]),
            "the fixture has no whole hole at level 1, so this test has stopped biting"
        );

        atlas.mark_alpha_tested(0);
        let levels = atlas.mips();
        for (level, image) in levels.iter().enumerate().skip(1) {
            for (x, y, texel) in tile_texels(image, level as u32) {
                assert_eq!(
                    [texel[0], texel[1], texel[2]],
                    LEAF,
                    "level {level} texel ({x}, {y}) is {texel:?} under alpha {}: a colour the \
                     filter will blend the leaf toward",
                    texel[3]
                );
            }
        }

        // And level 0 is the artist's image to the byte, holes black: the
        // interface draws icons from it through a premultiplied blend, where
        // a coloured hole would glow over the slot behind it.
        assert_eq!(levels[0], atlas.image);
    }

    #[test]
    fn dilation_touches_only_the_colour_of_clear_texels() {
        // What `dilate_tile_colour` may and may not change, checked on the
        // whole atlas rather than through the chain: no alpha byte moves, no
        // texel with any alpha moves, and every clear texel in the rect ends
        // with a colour from beside it — the leaf's, since that is the only
        // colour there is.
        let atlas = Atlas::build(&[Some(leaf_tile())]);
        let before = atlas.image.clone();
        let mut after = before.clone();
        dilate_tile_colour(&mut after, 0, 0);

        assert_eq!((after.width, after.height), (before.width, before.height));
        for y in 0..before.height {
            for x in 0..before.width {
                let (was, is) = (
                    before.pixel(x, y).expect("in bounds"),
                    after.pixel(x, y).expect("in bounds"),
                );
                assert_eq!(
                    was[3], is[3],
                    "alpha at ({x}, {y}) moved from {was:?} to {is:?}"
                );
                if was[3] > 0 {
                    assert_eq!(was, is, "a texel with alpha was rewritten at ({x}, {y})");
                } else {
                    assert_eq!(
                        [is[0], is[1], is[2]],
                        LEAF,
                        "clear texel ({x}, {y}) is {is:?}, not the colour beside it"
                    );
                }
            }
        }
    }

    #[test]
    fn a_fully_clear_tile_is_left_alone_by_dilation() {
        // Nothing to take a colour from, and a bounded loop that must still
        // stop: the pathological input for a fill that grows from its
        // coloured texels is a tile with none.
        let atlas = Atlas::build(&[Some(Image::solid(TILE, TILE, [0, 0, 0, 0]))]);
        let mut image = atlas.image.clone();
        dilate_tile_colour(&mut image, 0, 0);
        assert_eq!(image, atlas.image);
    }

    #[test]
    fn an_unmarked_tile_is_left_exactly_as_the_filter_made_it() {
        // Glass is BLENDED: its partial alpha is what a window looks like,
        // and "restoring" its coverage would darken every distant pane, while
        // colouring the clear black hole in this one would put a colour where
        // the artist drew none. Only marked tiles are touched, byte for byte.
        let mut pane = Image::solid(TILE, TILE, [200, 220, 255, 96]);
        for y in 6..10 {
            for x in 6..10 {
                let at = ((y * TILE + x) as usize) * 4;
                pane.rgba[at..at + 4].copy_from_slice(&[0, 0, 0, 0]);
            }
        }
        let atlas = Atlas::build(&[Some(pane)]);
        let mut expected = mip_chain(&atlas.image);
        expected.truncate(atlas.mip_levels() as usize);
        let levels = atlas.mips();
        assert_eq!(levels, expected);

        // Including the hole, which is still there and still black.
        let (x0, y0, _) = tile_rect(0, 0, 1);
        assert_eq!(
            levels[1].pixel(
                x0 + u32::midpoint(PADDING, 7),
                y0 + u32::midpoint(PADDING, 7)
            ),
            Some([0, 0, 0, 0]),
            "an unmarked tile's hole was coloured or filled"
        );
    }

    #[test]
    fn tile_uvs_exclude_the_padding() {
        // Sampling the padding would show the edge pixel stretched, which is a
        // subtle wrongness that looks like a texture authoring mistake.
        let atlas = Atlas::build(&vec![Some(Image::solid(TILE, TILE, [1, 2, 3, 4])); 4]);
        let (u0, v0, u1, v1) = atlas.tile_uv(0);

        let side = atlas.side() as f32;
        assert!(
            (u0 * side - PADDING as f32).abs() < 0.01,
            "u0 skips the padding"
        );
        assert!((v0 * side - PADDING as f32).abs() < 0.01);
        assert!(
            ((u1 - u0) * side - TILE as f32).abs() < 0.01,
            "spans one tile"
        );
        assert!(((v1 - v0) * side - TILE as f32).abs() < 0.01);
    }

    #[test]
    fn an_empty_atlas_is_still_valid() {
        let atlas = Atlas::build(&[]);
        assert!(atlas.side() > 0);
        assert_eq!(atlas.slot_of(0), 0);
    }

    #[test]
    fn an_atlas_counts_the_slots_that_hold_a_real_texture() {
        // **The number a player is shown when their world is drawn entirely in
        // the missing-texture chequer**, and the reason it has to come from the
        // atlas rather than from whatever was passed in: a slot is what a mesh
        // samples. An earlier version of this reading was a field on `App` that
        // nothing ever wrote, so it said "0 textured" on a working atlas as
        // readily as on a broken one — and it was the one number being used to
        // tell the two apart.
        let none = Atlas::build(&[None, None, None]);
        assert_eq!(none.filled(), 0);

        let some = Atlas::build(&[None, Some(Image::solid(4, 4, [255, 0, 0, 255])), None]);
        assert_eq!(some.filled(), 1);

        let all = Atlas::build(&[
            Some(Image::solid(4, 4, [1, 2, 3, 255])),
            Some(Image::solid(4, 4, [4, 5, 6, 255])),
        ]);
        assert_eq!(all.filled(), 2);
    }

    #[test]
    fn every_png_colour_type_decodes() {
        // A server can send any of these, and refusing one because it is
        // unusual means a texture pack that works elsewhere fails here.
        for colour in [
            png::ColorType::Rgba,
            png::ColorType::Rgb,
            png::ColorType::Grayscale,
            png::ColorType::GrayscaleAlpha,
        ] {
            let mut out = Vec::new();
            {
                let mut encoder = png::Encoder::new(&mut out, 8, 8);
                encoder.set_color(colour);
                encoder.set_depth(png::BitDepth::Eight);
                let mut writer = encoder.write_header().expect("header");
                let samples = match colour {
                    png::ColorType::Rgba => 4,
                    png::ColorType::Rgb => 3,
                    png::ColorType::GrayscaleAlpha => 2,
                    _ => 1,
                };
                writer
                    .write_image_data(&vec![200u8; 8 * 8 * samples])
                    .expect("data");
            }

            let image = decode_png(&out).unwrap_or_else(|err| panic!("{colour:?} failed: {err}"));
            assert_eq!(image.rgba.len(), 8 * 8 * 4, "{colour:?} should become RGBA");
        }
    }
}
