// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Riding: a player seated on an entity, driving it — Life ask 18.
//!
//! # A rider's keys move the mount, and the mount carries the rider
//!
//! While [`PlayerSim::riding`] is set the tick does not step the player's body
//! at all. Their intent, through [`tiamat_core::ent::mount::drive`], steps the
//! ENTITY — its own pace, its own box — through [`Population::drive`], which is
//! the one step every entity takes. Then the player's body is put at the seat,
//! turned with the mount, and the mount is left alone by the entity pass for
//! the rest of the tick. The rider's client predicts that same mount with the
//! same functions from the state its `PlayerState` carries: one simulation,
//! mirrored (charter rule 2).
//!
//! # Which way a mount faces
//!
//! **Where its rider looks**, set every tick it is driven, and the rider's own
//! body with it — so a seat off the mount's middle swings round as the rider
//! turns, and the horse's head is in front of the camera. The look is
//! presentation and never touches the physics (charter rule 4); the seat is
//! turned through `detgen::trig`, because it IS where the rider's body is.
//!
//! # Every way off, and where each lands
//!
//! - **Sneak**, when the seat allows it: here, before the mount moves, so the
//!   sneak never reaches the mount as a crawl. At the mount's feet.
//! - **`game.dismount`**: `ent::Shared`, at once. At the mount's feet.
//!   **`game.move_player`** on a rider is the same, where the move put them —
//!   otherwise the next tick would seat them again and the move would do
//!   nothing. (`game.push_player` on a rider pushes the mount instead, and they
//!   stay on.)
//! - **The mount despawned** — which is how a mod kills one: `ent::Shared`
//!   drops the rider in the same call, at the feet it stood on. Any other way
//!   it vanishes (a chunk unloaded under it) is noticed here the next tick, at
//!   the feet it last stood on.
//! - **Either moved into another domain**: noticed here the next tick. A
//!   mount that left lands its rider at its last feet in the rider's space; a
//!   rider who was moved stays where the move put them.
//! - **The player left**: noticed by the tick's roster diff, [`Rides`].
//!
//! Each is one [`DismountEvent`], heard by `on_dismount` after the entities
//! have moved. **The mount's feet are inside its box**, and a rider put there
//! is eased out of it by the crowd pass over the next second; a mod that wants
//! them beside the horse moves them from `on_dismount`, which is what the
//! event's position is for.
//!
//! # Nothing here is saved
//!
//! A ride is a property of a connected player's body, and a body is not
//! persisted — only an inventory is. **Leaving dismounts**, and a server
//! shutting down is everybody leaving: the mount is saved where it stands as
//! the ordinary entity it always was, and whoever comes back arrives on foot.
//!
//! # Cost
//!
//! A rider's tick is the entity's step instead of the player's — one
//! [`tiamat_core::phys::step_shaped`] either way — plus a copy and a turn of
//! the seat, and the mount is not stepped again by the entity pass. So a
//! mounted player costs one player's step and one entity's bookkeeping, less
//! than the player and the entity cost walking side by side.

use std::collections::{BTreeMap, BTreeSet};

use tiamat_core::PlayerUuid;
use tiamat_core::coords::ChunkPos;
use tiamat_core::ent::EntityId;
use tiamat_core::ent::mount::{self, Dismount, Refusal, Seat};
use tiamat_core::phys::{Body, Intent, Shape};
use tiamat_core::script::DismountEvent;

use crate::ent::Population;
use crate::transport::endpoint::PlayerSim;
use crate::world::World;

/// A player's ride: what they sit on, where, and the mount as last stepped.
///
/// **The mount's state here is a copy for the wire** — the entity in the store
/// is the authority, and every tick's step starts from it, so a mod moving the
/// horse moves the ride. The copy exists so that one lock gives a `PlayerState`
/// its input tick and the body that input drove together, and so a rider
/// whose mount has vanished still knows where it last stood.
#[derive(Debug, Clone, PartialEq)]
pub struct Riding {
    /// What they are riding.
    pub entity: EntityId,
    /// The space the ride is in: the rider's and the mount's when it began.
    pub domain: String,
    /// Where the rider's feet go, in cells from the mount's feet, in the
    /// mount's own frame — see [`mount::Seat`].
    pub seat: [f32; 3],
    /// Whether sneak gets them off.
    pub sneak_dismounts: bool,
    /// The chunk the mount's body is anchored to.
    pub origin: ChunkPos,
    /// The mount's body as last stepped.
    ///
    /// Its `jump_cooldown` is the one thing here that is the ride's own
    /// rather than a copy: an entity's store has no room for it, and a rider
    /// holding jump must be spaced as a player is, on both ends.
    pub body: Body,
    /// The mount's box.
    pub shape: Shape,
    /// The mount's pace.
    pub speed: f32,
}

impl Riding {
    /// The ride as the rider's `PlayerState` carries it: the body their
    /// client predicts, with everything it needs to step it as the tick does.
    #[must_use]
    pub const fn to_wire(&self) -> tiamat_core::proto::Riding {
        tiamat_core::proto::Riding {
            entity: self.entity.0,
            chunk: self.origin,
            local: self.body.position,
            velocity: self.body.velocity,
            on_ground: self.body.on_ground,
            jump_cooldown: self.body.jump_cooldown,
            size: [self.shape.width, self.shape.height],
            speed: self.speed,
            seat: self.seat,
        }
    }
}

/// Where a rider coming off is put.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Landing {
    /// At the mount's feet, as last stepped.
    Mount,
    /// Where the rider's body already is — a rider moved into another domain
    /// is not brought back to the one they left.
    Here,
}

/// What a tick's movement did with one player.
#[derive(Debug, Clone, PartialEq)]
pub enum Ride {
    /// They are not riding; their own body is the tick's to step.
    Walking,
    /// Their keys drove the mount and they are at the seat.
    Rode,
    /// They came off, and are where the event says.
    Off(DismountEvent),
}

/// Seats a connected player on an entity — `game.mount`'s rules.
///
/// Checked in this order, and the first that fails is the answer: connected,
/// the entity is live, it is not somebody's body, it has a box a body can be
/// stepped as, it is in the player's domain, nobody else is on it, and the
/// player is not already on something else. **Asking again for the entity
/// already ridden keeps the ride and moves the seat.**
///
/// # Errors
///
/// The [`Refusal`] that says why.
pub fn board(
    mobs: &Population,
    bodies: &mut BTreeMap<PlayerUuid, PlayerSim>,
    uuid: &PlayerUuid,
    id: EntityId,
    seat: Seat,
) -> Result<(), Refusal> {
    if !bodies.contains_key(uuid) {
        return Err(Refusal::NotConnected);
    }
    let entity = mobs.get(id).ok_or(Refusal::NoSuchEntity)?;
    // Somebody's body is moved by their own inputs, and riding one would be
    // two players' keys on one body — or a player's keys on their own.
    if mobs.player_of(id).is_some() || entity.source == crate::ent::PLAYER_SOURCE {
        return Err(Refusal::Player);
    }
    let shape = entity
        .collider
        .filter(|shape| mount::fits(*shape))
        .ok_or(Refusal::NoCollider)?;
    let domain = mobs.domain_of(id);
    let taken = bodies.iter().any(|(other, player)| {
        other != uuid && player.riding.as_ref().is_some_and(|ride| ride.entity == id)
    });
    let player = bodies.get_mut(uuid).ok_or(Refusal::NotConnected)?;
    if player.domain != domain {
        return Err(Refusal::OtherDomain);
    }
    // One rider to a mount. The seat is the only one, and two riders' keys on
    // one body would be the server choosing whose to believe.
    if taken {
        return Err(Refusal::Ridden);
    }
    if player.riding.as_ref().is_some_and(|ride| ride.entity != id) {
        return Err(Refusal::AlreadyRiding);
    }
    // Re-seating keeps the cooldown: the ride has not ended.
    let jump_cooldown = player
        .riding
        .as_ref()
        .map_or(0, |ride| ride.body.jump_cooldown);
    player.riding = Some(Riding {
        entity: id,
        domain: domain.to_owned(),
        seat: seat.resolve(shape),
        sneak_dismounts: seat.sneak_dismounts,
        origin: entity.transform.chunk,
        body: Body {
            position: entity.transform.local,
            velocity: entity.velocity.0,
            on_ground: entity.on_ground,
            jump_cooldown,
        },
        shape,
        speed: entity.speed,
    });
    // At the seat now rather than on the next tick, so no state is sent with
    // a rider standing beside what they are riding.
    seat_rider(player, entity.transform.yaw);
    // A ride is not a fall, whatever the body was doing a moment ago.
    player.falling = 0.0;
    Ok(())
}

/// Puts a rider's body at their seat, on their mount as last stepped.
///
/// The body takes the mount's velocity and footing, so the mirror everybody
/// else draws is moving as the mount is and stands on what it stands on.
pub fn seat_rider(player: &mut PlayerSim, yaw: f32) {
    let Some(ride) = player.riding.as_ref() else {
        return;
    };
    let offset = mount::seat_offset(ride.seat, yaw);
    let feet = [
        ride.body.position[0] + offset[0],
        ride.body.position[1] + offset[1],
        ride.body.position[2] + offset[2],
    ];
    // Charter rule 7: a seat can hang over a chunk plane, so the rider is
    // anchored to whichever chunk their own feet are in.
    let (origin, local) = tiamat_core::phys::voxels::renormalise(ride.origin, feet);
    player.origin = origin;
    player.body = Body {
        position: local,
        velocity: ride.body.velocity,
        on_ground: ride.body.on_ground,
        jump_cooldown: 0,
    };
}

/// Gets a rider off, and says so.
///
/// `None` if they were not riding. Their velocity is dropped — a rider off a
/// galloping horse stands rather than skids — and whatever fall they were
/// accruing with it, since a ride is not a fall.
pub fn alight(
    uuid: &PlayerUuid,
    player: &mut PlayerSim,
    reason: Dismount,
    landing: Landing,
) -> Option<DismountEvent> {
    let ride = player.riding.take()?;
    match landing {
        Landing::Mount => {
            player.origin = ride.origin;
            player.body = Body {
                on_ground: ride.body.on_ground,
                ..Body::at(ride.body.position)
            };
        }
        Landing::Here => {
            player.body.velocity = [0.0; 3];
        }
    }
    player.falling = 0.0;
    Some(DismountEvent {
        player: *uuid.as_bytes(),
        entity: ride.entity,
        reason,
        domain: player.domain.clone(),
        at: tiamat_core::ent::Transform::at(player.origin, player.body.position).to_world(),
    })
}

/// A rider's tick: their keys drive the mount, or get them off it.
///
/// `intent` is what their input queue gave for this tick, before any of the
/// rider's own abilities were applied — those are theirs, and the mount moves
/// at its own pace. `Walking` for a player who is not riding, whose body is
/// the caller's to step.
#[expect(
    clippy::too_many_arguments,
    reason = "a rider, their keys, and the world the mount steps in"
)]
pub fn ride(
    mobs: &mut Population,
    uuid: &PlayerUuid,
    player: &mut PlayerSim,
    intent: Intent,
    world: &World,
    fluid: &crate::fluid::Fluidics,
    passable: &[u16],
    friction: &[(u16, f32)],
) -> Ride {
    let Some(ride) = player.riding.as_ref() else {
        return Ride::Walking;
    };
    let off = |player: &mut PlayerSim, reason, landing| {
        alight(uuid, player, reason, landing).map_or(Ride::Walking, Ride::Off)
    };
    // The rider was moved into another space: the ride ends where they are.
    if player.domain != ride.domain {
        return off(player, Dismount::Gone, Landing::Here);
    }
    // Before the mount moves, so a sneak gets the rider off rather than
    // driving the horse at a crawl.
    if mount::sneaks_off(ride.sneak_dismounts, &intent) {
        return off(player, Dismount::Sneak, Landing::Mount);
    }
    // Where the rider looks, as a figure counts it: the conversion the
    // player's own mirror takes, so rider and mount face one way.
    let yaw = tiamat_core::ent::figure_yaw(player.look[0] * std::f32::consts::TAU);
    let Some(driven) = mobs.drive(
        ride.entity,
        &player.domain,
        world,
        fluid,
        (passable, friction),
        mount::drive(intent),
        ride.body.jump_cooldown,
        yaw,
    ) else {
        // Gone — despawned some other way than a mod's call, frozen with its
        // chunk, or moved into another domain. Where it last stood.
        return off(player, Dismount::Gone, Landing::Mount);
    };
    if let Some(ride) = player.riding.as_mut() {
        ride.origin = driven.origin;
        ride.body = driven.body;
        ride.shape = driven.shape;
        ride.speed = driven.speed;
    }
    seat_rider(player, yaw);
    Ride::Rode
}

/// Who was riding what, as of the last tick — for the one way off the tick
/// cannot see happen.
///
/// **A player who leaves takes their body with them**: the connection removes
/// it, and with it the ride, before the tick next looks. So the tick keeps
/// this note of every ride, and the roster diff that tells the mods somebody
/// left reads it to tell them the ride ended too.
#[derive(Debug, Default)]
pub struct Rides {
    last: BTreeMap<PlayerUuid, (EntityId, String, [f64; 3])>,
}

impl Rides {
    /// A note of nobody riding.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The rides of everybody who is no longer here, as `on_dismount` events.
    ///
    /// Read BEFORE [`Self::note`] replaces the record, since the record is
    /// the only place those rides still exist.
    pub fn departed(&mut self, present: &BTreeSet<PlayerUuid>) -> Vec<DismountEvent> {
        let gone: Vec<PlayerUuid> = self
            .last
            .keys()
            .filter(|uuid| !present.contains(*uuid))
            .copied()
            .collect();
        gone.into_iter()
            .filter_map(|uuid| {
                let (entity, domain, at) = self.last.remove(&uuid)?;
                Some(DismountEvent {
                    player: *uuid.as_bytes(),
                    entity,
                    reason: Dismount::Left,
                    domain,
                    at,
                })
            })
            .collect()
    }

    /// Records who is riding what now, and where they are.
    pub fn note(&mut self, bodies: &BTreeMap<PlayerUuid, PlayerSim>) {
        self.last.clear();
        for (uuid, player) in bodies {
            if let Some(ride) = player.riding.as_ref() {
                let at =
                    tiamat_core::ent::Transform::at(player.origin, player.body.position).to_world();
                self.last
                    .insert(*uuid, (ride.entity, player.domain.clone(), at));
            }
        }
    }
}

/// What a rider's body is drawn doing: sitting still, or swinging.
///
/// **Not the mount's gait.** The rider's body is carried at the mount's speed,
/// and reading a walk off that would have them striding in the saddle.
#[must_use]
pub const fn seated_anim(swinging: bool) -> tiamat_core::ent::AnimTag {
    if swinging {
        tiamat_core::ent::AnimTag::SWING
    } else {
        tiamat_core::ent::AnimTag::IDLE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiamat_core::MaterialId;
    use tiamat_core::domain::OVERWORLD;
    use tiamat_core::ent::{Entity, Transform};
    use tiamat_core::phys::Gait;

    const STONE: MaterialId = MaterialId(2);

    /// A generator that makes nothing, so a test decides the contents.
    struct Empty;

    impl crate::world::ChunkSource for Empty {
        fn generate(
            &mut self,
            _domain: &str,
            pos: ChunkPos,
            _seed: u64,
        ) -> tiamat_core::chunk::Chunk {
            tiamat_core::chunk::Chunk::air(pos)
        }
    }

    /// A world whose origin chunk has a stone floor in its bottom block.
    fn floored() -> World {
        let mut registry = tiamat_core::Registry::new();
        registry.register("test:stone").expect("register");
        let db = tiamat_core::persist::WorldDb::open_in_memory(&mut registry).expect("open");
        let mut world = World::open(db, 1).expect("world");
        world
            .chunk(OVERWORLD, ChunkPos::new(0, 0, 0), &mut Empty)
            .expect("chunk");
        for x in 0..16 {
            for z in 0..16 {
                world
                    .apply(
                        OVERWORLD,
                        &tiamat_core::proto::Edit::Block {
                            pos: tiamat_core::BlockPos::new(x, 0, z),
                            material: STONE.get(),
                        },
                        &mut Empty,
                    )
                    .expect("place");
            }
        }
        world
    }

    /// A horse-sized creature standing on the floor, at a pace of its own.
    fn horse(local: [f32; 3], speed: f32) -> Entity {
        Entity {
            collider: Some(Shape {
                width: 3.0,
                height: 4.5,
            }),
            speed,
            on_ground: true,
            ..Entity::at(Transform::at(ChunkPos::new(0, 0, 0), local), "test:horse")
        }
    }

    fn rider() -> (PlayerUuid, PlayerSim) {
        let uuid = tiamat_core::identity::Identity::generate()
            .expect("identity")
            .uuid_as_root();
        (
            uuid,
            PlayerSim::spawned_at(tiamat_core::BlockPos::new(2, 1, 2), 0),
        )
    }

    fn north(gait: Gait, jump: bool) -> Intent {
        Intent {
            walk: [0.0, 1.0],
            jump,
            gait,
            fly: false,
        }
    }

    const SEAT: Seat = Seat {
        offset: Some([0.0, 4.8, 0.0]),
        sneak_dismounts: true,
    };

    /// Seats `player` on `id`, through the same rules `game.mount` takes.
    fn seat_on(
        mobs: &Population,
        uuid: &PlayerUuid,
        player: &mut PlayerSim,
        id: EntityId,
        seat: Seat,
    ) {
        let mut bodies = BTreeMap::new();
        bodies.insert(*uuid, player.clone());
        board(mobs, &mut bodies, uuid, id, seat).expect("mounted");
        *player = bodies.remove(uuid).expect("still there");
    }

    /// One tick of the players' pass for one rider, with nothing in the way.
    fn ride_once(
        mobs: &mut Population,
        uuid: &PlayerUuid,
        player: &mut PlayerSim,
        intent: Intent,
        world: &World,
    ) -> Ride {
        mobs.begin_rides();
        let dry = crate::fluid::Fluidics::default();
        ride(mobs, uuid, player, intent, world, &dry, &[], &[])
    }

    #[test]
    fn a_riders_keys_step_the_mount_exactly_as_its_own_drive_would() {
        // **The same step, not a copy of it.** A ridden horse and an unridden
        // one driven by the same keys, from the same place, must come out bit
        // for bit the same — with the mount's own pace, which is half here —
        // and the rider must sit at the seat on top of it every tick.
        let world = floored();
        let dry = crate::fluid::Fluidics::default();
        let mut mobs = Population::new();
        let ridden = mobs.spawn(horse([8.0, 3.0, 4.0], 0.5));
        let mut alone = Population::new();
        let free = alone.spawn(Entity {
            drive: north(Gait::Walk, false),
            ..horse([8.0, 3.0, 4.0], 0.5)
        });
        let (uuid, mut player) = rider();
        seat_on(&mobs, &uuid, &mut player, ridden, SEAT);

        for tick in 0..40 {
            assert_eq!(
                ride_once(
                    &mut mobs,
                    &uuid,
                    &mut player,
                    north(Gait::Walk, false),
                    &world
                ),
                Ride::Rode,
                "tick {tick}"
            );
            // The entity pass after it must leave the mount alone: it has
            // had its step.
            mobs.tick(OVERWORLD, &world, &dry, &[], &[]);
            alone.tick(OVERWORLD, &world, &dry, &[], &[]);

            let mount = mobs.get(ridden).expect("the mount");
            let twin = alone.get(free).expect("the twin");
            assert_eq!(
                mount.transform.local.map(f32::to_bits),
                twin.transform.local.map(f32::to_bits),
                "tick {tick}: the ridden horse and its twin parted"
            );
            assert_eq!(mount.transform.chunk, twin.transform.chunk);
            let seat = Transform::at(player.origin, player.body.position);
            let under = mount.transform.offset_to(&seat);
            assert!(
                under[0].abs() < 1e-4 && (under[1] - 4.8).abs() < 1e-4 && under[2].abs() < 1e-4,
                "tick {tick}: the rider is {under:?} from the horse's feet"
            );
        }
        let walked = mobs.get(ridden).expect("the mount").transform.local[2] - 4.0;
        assert!(
            walked > 1.0,
            "the horse never went anywhere: {walked} cells"
        );

        // And at its own pace, not the rider's: half a pace covers half the
        // ground an ordinary one does over the same keys.
        let mut ordinary = Population::new();
        let quick = ordinary.spawn(Entity {
            drive: north(Gait::Walk, false),
            ..horse([8.0, 3.0, 4.0], 1.0)
        });
        for _ in 0..40 {
            ordinary.tick(OVERWORLD, &world, &dry, &[], &[]);
        }
        let quick = ordinary.get(quick).expect("there").transform.local[2] - 4.0;
        assert!(
            (walked / quick - 0.5).abs() < 0.05,
            "the ridden horse walked {walked} against an ordinary {quick}"
        );
    }

    #[test]
    fn a_held_jump_is_spaced_on_a_mount_as_it_is_on_foot() {
        // The cooldown is the ride's own — an entity's store has none — so a
        // rider holding jump launches the horse once per cooldown, not on
        // every tick it touches the ground.
        let world = floored();
        let mut mobs = Population::new();
        let id = mobs.spawn(horse([8.0, 3.0, 8.0], 1.0));
        let (uuid, mut player) = rider();
        seat_on(&mobs, &uuid, &mut player, id, SEAT);

        let mut launches = 0;
        let mut was = mobs.get(id).expect("there").velocity.0[1];
        let ticks = 60;
        for _ in 0..ticks {
            ride_once(
                &mut mobs,
                &uuid,
                &mut player,
                north(Gait::Walk, true),
                &world,
            );
            let now = mobs.get(id).expect("there").velocity.0[1];
            if now > 0.0 && was <= 0.0 {
                launches += 1;
            }
            was = now;
        }
        let cooldown = u32::from(tiamat_core::phys::Tuning::DEFAULT.jump_cooldown_ticks);
        assert!(launches >= 2, "the horse jumped {launches} times");
        assert!(
            launches <= ticks / cooldown + 1,
            "{launches} jumps in {ticks} ticks with a {cooldown}-tick cooldown"
        );
    }

    #[test]
    fn every_mount_the_rules_refuse_is_refused_with_its_reason() {
        let mut mobs = Population::new();
        let (uuid, player) = rider();
        let (other, second) = rider();
        let mut bodies = BTreeMap::new();
        bodies.insert(uuid, player);
        bodies.insert(other, second);
        let horse_id = mobs.spawn(horse([8.0, 3.0, 8.0], 1.0));

        // Nobody by that name.
        let (stranger, _) = rider();
        assert_eq!(
            board(&mobs, &mut bodies, &stranger, horse_id, SEAT),
            Err(Refusal::NotConnected)
        );
        // Nothing by that id — a stale one included.
        let gone = mobs.spawn(horse([4.0, 3.0, 4.0], 1.0));
        mobs.despawn(gone);
        assert_eq!(
            board(&mobs, &mut bodies, &uuid, gone, SEAT),
            Err(Refusal::NoSuchEntity)
        );
        // Somebody's body.
        let body = mobs.sync_player(
            other,
            Transform::at(ChunkPos::new(0, 0, 0), [20.0, 3.0, 20.0]),
            crate::ent::Motion::default(),
            tiamat_core::ent::AnimTag::IDLE,
            tiamat_core::ent::Hands::default(),
        );
        assert_eq!(
            board(&mobs, &mut bodies, &uuid, body, SEAT),
            Err(Refusal::Player)
        );
        // A marker with no box, and a box no body can be stepped as.
        let marker = mobs.spawn(Entity::at(
            Transform::at(ChunkPos::new(0, 0, 0), [4.0, 3.0, 4.0]),
            "test:marker",
        ));
        assert_eq!(
            board(&mobs, &mut bodies, &uuid, marker, SEAT),
            Err(Refusal::NoCollider)
        );
        let barge = mobs.spawn(Entity {
            collider: Some(Shape {
                width: mount::MAX_MOUNT_CELLS * 2.0,
                height: 3.0,
            }),
            ..horse([4.0, 3.0, 4.0], 1.0)
        });
        assert_eq!(
            board(&mobs, &mut bodies, &uuid, barge, SEAT),
            Err(Refusal::NoCollider)
        );
        // Another space.
        let shipped = mobs.spawn(horse([4.0, 3.0, 4.0], 1.0));
        mobs.set_domain(shipped, "mod:ship/1");
        assert_eq!(
            board(&mobs, &mut bodies, &uuid, shipped, SEAT),
            Err(Refusal::OtherDomain)
        );

        // One rider to a mount: the second is refused, and the first asking
        // again keeps the ride and moves the seat.
        board(&mobs, &mut bodies, &uuid, horse_id, SEAT).expect("the first");
        assert_eq!(
            board(&mobs, &mut bodies, &other, horse_id, SEAT),
            Err(Refusal::Ridden)
        );
        let lower = Seat {
            offset: Some([0.0, 3.0, -1.5]),
            ..SEAT
        };
        board(&mobs, &mut bodies, &uuid, horse_id, lower).expect("re-seated");
        assert_eq!(
            bodies[&uuid].riding.as_ref().map(|ride| ride.seat),
            Some([0.0, 3.0, -1.5])
        );
        // And already on one, a second is refused until they get off.
        let pony = mobs.spawn(horse([30.0, 3.0, 30.0], 1.0));
        assert_eq!(
            board(&mobs, &mut bodies, &uuid, pony, SEAT),
            Err(Refusal::AlreadyRiding)
        );
        let player = bodies.get_mut(&uuid).expect("there");
        assert!(alight(&uuid, player, Dismount::Asked, Landing::Mount).is_some());
        board(&mobs, &mut bodies, &uuid, pony, SEAT).expect("free to change mounts now");
    }

    #[test]
    fn a_seat_named_nowhere_puts_the_rider_on_top_of_the_box() {
        let mut mobs = Population::new();
        let id = mobs.spawn(horse([8.0, 3.0, 8.0], 1.0));
        let (uuid, mut player) = rider();
        seat_on(&mobs, &uuid, &mut player, id, Seat::default());
        let feet = Transform::at(player.origin, player.body.position);
        let under = mobs.get(id).expect("there").transform.offset_to(&feet);
        assert!((under[1] - 4.5).abs() < 1e-4, "{under:?}");
    }

    #[test]
    fn sneak_gets_a_rider_off_at_the_mounts_feet_before_it_moves() {
        let world = floored();
        let mut mobs = Population::new();
        let id = mobs.spawn(horse([8.0, 3.0, 8.0], 1.0));
        let (uuid, mut player) = rider();
        seat_on(&mobs, &uuid, &mut player, id, SEAT);
        let before = mobs.get(id).expect("there").transform;

        let Ride::Off(event) = ride_once(
            &mut mobs,
            &uuid,
            &mut player,
            north(Gait::Sneak, false),
            &world,
        ) else {
            panic!("sneak did not get them off");
        };
        assert_eq!(event.reason, Dismount::Sneak);
        assert_eq!(event.entity, id);
        assert!(player.riding.is_none());
        // The sneak never reached the horse, and the rider stands where it does.
        let horse = mobs.get(id).expect("there").transform;
        assert_eq!(horse, before, "the sneak drove the horse");
        let feet = Transform::at(player.origin, player.body.position);
        assert!(horse.offset_to(&feet).iter().all(|axis| axis.abs() < 1e-4));
        let at = horse.to_world();
        assert!(
            event
                .at
                .iter()
                .zip(at)
                .all(|(heard, stood)| (heard - stood).abs() < 1e-6),
            "heard {:?}, stood at {at:?}",
            event.at
        );

        // A seat that keeps its rider through a sneak drives the horse at a
        // crawl instead.
        let sticky = Seat {
            sneak_dismounts: false,
            ..SEAT
        };
        seat_on(&mobs, &uuid, &mut player, id, sticky);
        assert_eq!(
            ride_once(
                &mut mobs,
                &uuid,
                &mut player,
                north(Gait::Sneak, false),
                &world
            ),
            Ride::Rode
        );
        assert!(player.riding.is_some());
    }

    #[test]
    fn a_mount_that_vanishes_or_parts_from_its_rider_drops_them() {
        let world = floored();
        let mut mobs = Population::new();
        let (uuid, mut player) = rider();

        // Despawned out from under them some other way than a mod's call:
        // at the feet it last stood on.
        let id = mobs.spawn(horse([8.0, 3.0, 8.0], 1.0));
        seat_on(&mobs, &uuid, &mut player, id, SEAT);
        ride_once(
            &mut mobs,
            &uuid,
            &mut player,
            north(Gait::Walk, false),
            &world,
        );
        let last = mobs.get(id).expect("there").transform;
        mobs.despawn(id);
        let Ride::Off(event) = ride_once(
            &mut mobs,
            &uuid,
            &mut player,
            north(Gait::Walk, false),
            &world,
        ) else {
            panic!("a vanished mount carried its rider");
        };
        assert_eq!(event.reason, Dismount::Gone);
        let feet = Transform::at(player.origin, player.body.position);
        assert!(last.offset_to(&feet).iter().all(|axis| axis.abs() < 1e-4));

        // Moved into another space: the same.
        let id = mobs.spawn(horse([8.0, 3.0, 8.0], 1.0));
        seat_on(&mobs, &uuid, &mut player, id, SEAT);
        mobs.set_domain(id, "mod:ship/1");
        assert!(matches!(
            ride_once(
                &mut mobs,
                &uuid,
                &mut player,
                north(Gait::Walk, false),
                &world
            ),
            Ride::Off(DismountEvent {
                reason: Dismount::Gone,
                ..
            })
        ));

        // The rider moved into another space: off, and left where they are.
        let id = mobs.spawn(horse([8.0, 3.0, 8.0], 1.0));
        seat_on(&mobs, &uuid, &mut player, id, SEAT);
        player.domain = "mod:ship/1".to_owned();
        let there = (player.origin, player.body.position);
        let Ride::Off(event) = ride_once(
            &mut mobs,
            &uuid,
            &mut player,
            north(Gait::Walk, false),
            &world,
        ) else {
            panic!("a rider in another space still rode");
        };
        assert_eq!(event.reason, Dismount::Gone);
        assert_eq!(event.domain, "mod:ship/1");
        assert_eq!((player.origin, player.body.position), there);
    }

    #[test]
    fn a_player_who_leaves_while_riding_is_heard_as_a_leave() {
        let mut mobs = Population::new();
        let id = mobs.spawn(horse([8.0, 3.0, 8.0], 1.0));
        let (uuid, player) = rider();
        let (walker, on_foot) = rider();
        let mut bodies = BTreeMap::new();
        bodies.insert(uuid, player);
        bodies.insert(walker, on_foot);
        board(&mobs, &mut bodies, &uuid, id, SEAT).expect("mounted");

        let mut rides = Rides::new();
        let everyone: BTreeSet<PlayerUuid> = bodies.keys().copied().collect();
        assert!(rides.departed(&everyone).is_empty());
        rides.note(&bodies);

        // Both go; only the rider's going ends a ride.
        let left = rides.departed(&BTreeSet::new());
        assert_eq!(left.len(), 1, "{left:?}");
        assert_eq!(left[0].player, *uuid.as_bytes());
        assert_eq!(left[0].entity, id);
        assert_eq!(left[0].reason, Dismount::Left);
        // And once.
        assert!(rides.departed(&BTreeSet::new()).is_empty());

        // With them gone the horse is anybody's.
        bodies.remove(&uuid);
        board(&mobs, &mut bodies, &walker, id, SEAT).expect("the horse is free");
    }

    #[test]
    fn a_mods_calls_get_a_rider_off_at_once_and_are_heard_later() {
        use tiamat_core::ent::Access as _;
        let population = std::sync::Arc::new(std::sync::RwLock::new(Population::new()));
        let bodies = std::sync::Arc::new(crate::transport::PlayerBodies::default());
        let access = crate::ent::Shared::new(
            std::sync::Arc::clone(&population),
            std::sync::Arc::clone(&bodies),
            std::sync::Arc::new(std::sync::RwLock::new(tiamat_core::domain::Registry::new())),
        );
        let (uuid, player) = rider();
        bodies.lock().expect("bodies").insert(uuid, player);
        let first = population
            .write()
            .expect("population")
            .spawn(horse([8.0, 3.0, 8.0], 1.0));

        access
            .mount(*uuid.as_bytes(), first, SEAT)
            .expect("mounted");
        assert_eq!(access.mounted(*uuid.as_bytes()), Some(first));
        assert!(access.dismount(*uuid.as_bytes()));
        assert!(!access.dismount(*uuid.as_bytes()), "off twice");
        assert_eq!(access.mounted(*uuid.as_bytes()), None);

        // A despawn under a rider drops them in the same call.
        access
            .mount(*uuid.as_bytes(), first, SEAT)
            .expect("mounted again");
        assert!(access.despawn(first));
        assert_eq!(access.mounted(*uuid.as_bytes()), None);

        // A push on a rider moves what carries them, and the rider stays on.
        let second = population
            .write()
            .expect("population")
            .spawn(horse([8.0, 3.0, 8.0], 1.0));
        access
            .mount(*uuid.as_bytes(), second, SEAT)
            .expect("mounted on the second");
        assert!(access.shove_player(*uuid.as_bytes(), [0.5, 0.25, 0.0]));
        let pushed = population
            .read()
            .expect("population")
            .get(second)
            .expect("there")
            .clone();
        assert!(
            (pushed.velocity.0[0] - 0.5).abs() < f32::EPSILON && !pushed.on_ground,
            "the horse was not knocked: {:?}",
            pushed.velocity
        );
        assert_eq!(access.mounted(*uuid.as_bytes()), Some(second));

        // A rider moved is a rider off, where the move put them — otherwise
        // the next tick would seat them again and the move would do nothing.
        assert!(access.move_player(*uuid.as_bytes(), [40.5, 12.0, -7.5]));
        assert_eq!(access.mounted(*uuid.as_bytes()), None);

        let heard: Vec<(Dismount, [f64; 3])> = access
            .take_dismounts()
            .iter()
            .map(|event| (event.reason, event.at))
            .collect();
        let reasons: Vec<Dismount> = heard.iter().map(|(reason, _)| *reason).collect();
        assert_eq!(
            reasons,
            vec![Dismount::Asked, Dismount::Gone, Dismount::Asked]
        );
        let moved_to = heard[2].1;
        assert!(
            (moved_to[0] - 40.5).abs() < 1e-3
                && (moved_to[1] - 12.0).abs() < 1e-3
                && (moved_to[2] + 7.5).abs() < 1e-3,
            "a moved rider was heard at {moved_to:?}"
        );
        assert!(access.take_dismounts().is_empty(), "heard twice");
    }
}
