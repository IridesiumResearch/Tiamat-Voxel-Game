// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Where `game.set_sky_modifier`, `game.flash` and `game.lightning` reach:
//! the connected players.

use std::sync::Arc;

use tiamat_core::atmosphere::{Clouds, Precipitation, SkyModifier};
use tiamat_core::identity::PlayerUuid;

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

impl Shared {
    /// Queues `message` for everyone within `radius` of `pos` in `domain` —
    /// or, with `player`, for that one alone if they are — and answers how
    /// many.
    ///
    /// The flash's and the bolt's addressing, shared: one rule for who sees
    /// the weather, so a bolt can never reach a player its own flash missed.
    fn tell_in_reach(
        &self,
        domain: &str,
        pos: [f64; 3],
        radius: f32,
        player: Option<PlayerUuid>,
        message: &tiamat_core::proto::ServerMessage,
    ) -> u32 {
        let Ok(bodies) = self.endpoint.bodies.lock() else {
            return 0;
        };
        let radius = f64::from(radius);
        let mut told = 0;
        for (uuid, state) in bodies.iter() {
            if state.domain != domain {
                continue;
            }
            // Addressed to one player: everyone else is skipped, and that
            // one still has to be in range (W28).
            if player.is_some_and(|only| only != *uuid) {
                continue;
            }
            let at = tiamat_core::ent::Transform::at(state.origin, state.body.position).to_world();
            let offset = [at[0] - pos[0], at[1] - pos[1], at[2] - pos[2]];
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

impl tiamat_core::atmosphere::Access for Shared {
    fn set_sky_modifier(&self, player: PlayerUuid, modifier: Option<SkyModifier>) -> bool {
        // Whether the player is here, not whether the write happened — as
        // the HUD has it, and for its reason.
        if !self.endpoint.is_online(&player) {
            return false;
        }
        self.endpoint.set_sky_modifier(&player, modifier);
        true
    }

    fn flash(&self, request: &tiamat_core::atmosphere::FlashRequest) -> u32 {
        // **Who can see it, decided here and not by the client**, as a
        // sound's earshot is: the domain and the radius, and a client told
        // about every strike on the server would light up for storms it is
        // not under.
        let message = tiamat_core::proto::ServerMessage::Flash {
            flash: request.flash,
        };
        self.tell_in_reach(
            &request.domain,
            request.pos,
            request.radius,
            request.player,
            &message,
        )
    }

    fn lightning(&self, request: &tiamat_core::lightning::LightningRequest) -> u32 {
        // Addressed exactly as the flash beside it is (weather ask W26), and
        // measured from the top: a bolt is seen from wherever its storm is,
        // and the top is the end a mod places by the storm.
        let message = tiamat_core::proto::ServerMessage::Lightning {
            lightning: request.lightning,
        };
        self.tell_in_reach(
            &request.domain,
            request.lightning.from,
            request.radius,
            request.player,
            &message,
        )
    }

    fn set_precipitation(&self, player: PlayerUuid, precipitation: Option<Precipitation>) -> bool {
        if !self.endpoint.is_online(&player) {
            return false;
        }
        self.endpoint.set_precipitation(&player, precipitation);
        true
    }

    fn set_clouds(&self, player: PlayerUuid, clouds: Option<Clouds>) -> bool {
        if !self.endpoint.is_online(&player) {
            return false;
        }
        self.endpoint.set_clouds(&player, clouds);
        true
    }

    fn set_cloud_map(
        &self,
        player: PlayerUuid,
        map: Option<tiamat_core::atmosphere::CloudMap>,
    ) -> bool {
        if !self.endpoint.is_online(&player) {
            return false;
        }
        self.endpoint.set_cloud_map(&player, map);
        true
    }
}
