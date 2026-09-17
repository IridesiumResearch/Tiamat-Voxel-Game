// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Pictures a mod registers so a client fetches them before a HUD names them.
//!
//! # Why a picture needs registering at all
//!
//! A client asks for content by hash, and something has to tell it which
//! hashes. A dialog's tree is its own manifest — every `image` and every
//! `nine_slice` in it is asked for when the tree arrives — but a HUD script
//! names a picture only when it draws one, by which time the frame is being
//! painted, and nothing had ever asked for the bytes: every `hud.image` drew
//! the magenta "not arrived" box for ever. `game.register_picture` puts the
//! file in a table the client fetches from on join, the way it fetches sounds
//! and fonts, and answers the content hash so a mod need not paste one by hand.

/// A picture a mod registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    /// The qualified id, e.g. `"my_mod:hotbar_slot"`.
    pub id: String,
    /// The mod that registered it.
    pub mod_id: String,
    /// The file, relative to the mod's own directory.
    pub file: String,
}

/// How many pictures a server may push in its table.
///
/// A HUD's icons and a few frames are a few dozen; this is generous for any
/// real mod set and finite against a hostile server, which is what a cap on a
/// table of hashes is for — the bytes are bounded where they arrive.
pub const MAX_PICTURES: usize = 512;
