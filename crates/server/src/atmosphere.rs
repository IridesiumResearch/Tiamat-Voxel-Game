// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Where `game.set_sky_modifier` reaches: the connected players.

use std::sync::Arc;

use tiamot_core::atmosphere::SkyModifier;
use tiamot_core::identity::PlayerUuid;

/// The atmosphere seam over the connection state, as [`crate::hud::Shared`].
#[derive(Clone)]
pub struct Shared {
    endpoint: Arc<crate::transport::Shared>,
}

impl Shared {
    /// Wraps the connection state the modifiers are kept on.
    #[must_use]
    pub const fn new(endpoint: Arc<crate::transport::Shared>) -> Self {
        Self { endpoint }
    }
}

impl tiamot_core::atmosphere::Access for Shared {
    fn set_sky_modifier(&self, player: PlayerUuid, modifier: Option<SkyModifier>) -> bool {
        // Whether the player is here, not whether the write happened — as
        // the HUD has it, and for its reason.
        if !self.endpoint.is_online(&player) {
            return false;
        }
        self.endpoint.set_sky_modifier(&player, modifier);
        true
    }
}
