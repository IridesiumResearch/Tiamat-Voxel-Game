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

    fn flash(&self, request: &tiamot_core::atmosphere::FlashRequest) -> u32 {
        // **Who can see it, decided here and not by the client**, as a
        // sound's earshot is: the domain and the radius, and a client told
        // about every strike on the server would light up for storms it is
        // not under.
        let Ok(bodies) = self.endpoint.bodies.lock() else {
            return 0;
        };
        let message = tiamot_core::proto::ServerMessage::Flash {
            flash: request.flash,
        };
        let radius = f64::from(request.radius);
        let mut told = 0;
        for (uuid, player) in bodies.iter() {
            if player.domain != request.domain {
                continue;
            }
            let at =
                tiamot_core::ent::Transform::at(player.origin, player.body.position).to_world();
            let offset = [
                at[0] - request.pos[0],
                at[1] - request.pos[1],
                at[2] - request.pos[2],
            ];
            let distance = offset[0] * offset[0] + offset[1] * offset[1] + offset[2] * offset[2];
            if distance > radius * radius {
                continue;
            }
            let _ = self
                .endpoint
                .push_entity_messages(uuid, std::iter::once(message.clone()));
            told += 1;
        }
        told
    }
}
