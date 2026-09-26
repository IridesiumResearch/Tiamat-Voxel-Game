// SPDX-FileCopyrightText: Iridesium
// SPDX-License-Identifier: GPL-3.0-only

//! Using an entity, over a real server — Life ask 17.
//!
//! The place control with a creature under the crosshair reaches
//! `register_on_use_entity` with the entity, its owner and the hand; the
//! server cast the ray, so the client named nothing. `game.looking_at` answers
//! the same entity. And a use nobody handles falls through to what it always
//! was, so a world with no such mod plays as it did.

use std::path::{Path, PathBuf};
use std::time::Duration;

use bot::Bot;
use tiamat_core::BlockPos;
use tiamat_core::identity::{Allowlist, Identity};
use tiamat_core::interest::ViewDistance;
use tiamat_server::{ServerHandle, Settings};

const PATIENCE: Duration = Duration::from_secs(30);

/// Marks the mod raises, at places nothing generated could be mistaken for.
const USED_ENTITY: BlockPos = BlockPos::new(1, 10, 1);
const NO_OWNER: BlockPos = BlockPos::new(3, 10, 1);
const USED_BLOCK: BlockPos = BlockPos::new(5, 10, 1);
const LOOKED_AT_IT: BlockPos = BlockPos::new(7, 10, 1);
const SCARECROW_UP: BlockPos = BlockPos::new(9, 10, 1);

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("tiamat-use-entity").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Flat ground, a hand, and a scarecrow two blocks north of wherever a player
/// stands when they arrive — in reach, in front of the sky, so the only thing
/// on the ray is the scarecrow. `handles` is the `on_use_entity` body.
fn write_field(root: &Path, handles: &str) -> PathBuf {
    let mods = root.join("mods");
    let dir = mods.join("field");
    std::fs::create_dir_all(&dir).expect("mod dir");
    std::fs::write(
        dir.join("mod.toml"),
        "id = \"field\"\nname = \"Field\"\nversion = \"0.1.0\"\nlicense = \"GPL-3.0-only\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("init.lua"),
        format!(
            r#"
local ground = game.register_block{{ id = "ground" }}
game.register_block{{ id = "mark" }}
game.register_on_generate(function(buf, pos)
    buf:fill_below_heightmap(game.flat_heightmap(0), ground)
end)
game.register_tool{{ id = "hand", brush = "block", speed_multiplier = 1.0, default = true }}

-- One scarecrow per player, put up the first tick their body is there.
local placed = {{}}
game.register_on_player_join(function(event)
    placed[event.player] = false
end)
game.register_on_tick(function()
    for uuid, done in pairs(placed) do
        if not done then
            local id = game.player_entity(uuid)
            local body = id and game.entity(id)
            if body then
                -- A model, so the client is told about it and the test can
                -- see it stand: an entity nobody can see is still a target,
                -- but a test wants to watch.
                local scarecrow = game.spawn_entity{{
                    pos = {{ x = body.pos.x, y = body.pos.y, z = body.pos.z + 2 }},
                    model = "engine:humanoid",
                    collider = {{ width = 2.4, height = 6 }},
                }}
                if scarecrow ~= nil then
                    placed[uuid] = true
                    game.set_block({{ x = 9, y = 10, z = 1 }}, "field:mark")
                end
            end
        end
    end
end)

game.register_on_use_entity(function(event)
    game.set_block({{ x = 1, y = 10, z = 1 }}, "field:mark")
    if event.owner == nil then
        game.set_block({{ x = 3, y = 10, z = 1 }}, "field:mark")
    end
    {handles}
end)

-- Never for the scarecrow: a handled use of an entity stops here.
game.register_on_use(function(event)
    game.set_block({{ x = 5, y = 10, z = 1 }}, "field:mark")
    return ""
end, {{ anywhere = true }})

game.register_on_chat(function(event)
    if event.text == "look" then
        local at = game.looking_at(event.player)
        if at and at.entity then
            game.set_block({{ x = 7, y = 10, z = 1 }}, "field:mark")
        end
        -- What it saw, in words, for a failing test to read.
        game.chat_to(event.player, "look: " .. (at == nil and "nothing" or (at.entity and ("entity " .. at.entity) or ("cell " .. at.x .. "," .. at.y .. "," .. at.z))))
        return false
    end
end)
"#
        ),
    )
    .expect("script");
    mods
}

fn start(name: &str, handles: &str) -> ServerHandle {
    let root = scratch(name);
    let mods = write_field(&root, handles);
    ServerHandle::start(&Settings {
        bind_addr: "127.0.0.1:0".parse().expect("loopback"),
        world_path: root.join("world"),
        identity_path: None,
        max_players: 4,
        allowlist: Allowlist::open(),
        operators: Vec::new(),
        view_distance: ViewDistance::MINIMUM,
        mods_path: Some(mods),
        enabled_mods: None,
        seed: Some(19),
        rcon: None,
        materials: Vec::new(),
        world_options: Vec::new(),
    })
    .expect("start")
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(future)
}

async fn join(server: &ServerHandle, name: &str) -> Bot {
    let mut bot = Bot::connect(
        server.local_addr(),
        Identity::generate().expect("identity"),
        server.cert_fingerprint(),
    )
    .await
    .expect("connect");
    bot.join(name).await.expect("join");
    bot
}

fn mark_id(bot: &Bot) -> u16 {
    bot.material_table()
        .expect("a material table on join")
        .into_iter()
        .find(|entry| entry.name == "field:mark")
        .map(|entry| entry.id)
        .expect("the mod registers a mark")
}

/// Waits for the scarecrow: the mod's mark that it stood one up.
async fn scarecrow_up(bot: &mut Bot, mark: u16) {
    bot.expect_block(SCARECROW_UP, mark, PATIENCE)
        .await
        .expect("the mod should put a scarecrow up");
    // And a few ticks for it to land and for the server's picture of the
    // player to settle, so the ray is cast from where they stand.
    bot.sleep_ticks(6).await;
}

#[test]
fn using_a_creature_reaches_the_mod_with_the_entity_and_stops_the_block_use() {
    let server = start("feed", "return \"\"");
    block_on(async {
        let mut bot = join(&server, "Farmer").await;
        let mark = mark_id(&bot);
        scarecrow_up(&mut bot, mark).await;

        // A use at nothing from the client's point of view — the sky is
        // behind the scarecrow — and the server's ray says otherwise.
        bot.chat("look").await.expect("send");
        bot.use_at_nothing().await.expect("send");
        if bot.expect_block(USED_ENTITY, mark, PATIENCE).await.is_err() {
            panic!(
                "the mod did not hear the use of the scarecrow; notices {:?}",
                bot.notices()
            );
        }
        bot.expect_block(NO_OWNER, mark, PATIENCE)
            .await
            .expect("a scarecrow has no owner");
        bot.sleep_ticks(4).await;
        assert!(
            !bot.saw_block(USED_BLOCK, mark),
            "a handled use of an entity went on to the block use"
        );

        // And what the mod sees the player looking at is the same thing.
        bot.chat("look").await.expect("send");
        bot.expect_block(LOOKED_AT_IT, mark, PATIENCE)
            .await
            .expect("looking_at should answer the scarecrow");
    });
    assert!(server.stop());
}

#[test]
fn a_use_of_a_creature_nobody_handles_falls_through_to_what_it_always_was() {
    let server = start("ignore", "return nil");
    block_on(async {
        let mut bot = join(&server, "Passer").await;
        let mark = mark_id(&bot);
        scarecrow_up(&mut bot, mark).await;

        bot.use_at_nothing().await.expect("send");
        bot.expect_block(USED_ENTITY, mark, PATIENCE)
            .await
            .expect("the mod should still hear the use");
        // Let pass, so the use goes on: at nothing, which the mod's
        // `anywhere` handler takes, exactly as before the hook existed.
        bot.expect_block(USED_BLOCK, mark, PATIENCE)
            .await
            .expect("an unhandled use of an entity should fall through");
    });
    assert!(server.stop());
}

#[test]
fn a_bare_false_from_the_hook_is_told_in_the_engines_words_like_a_block_use() {
    // The same ladder as `register_on_use`, read the same way: `false` handles
    // it and the player hears the engine's wording, which for an empty hand
    // is the one it has always had.
    let server = start("refuse", "return false");
    block_on(async {
        let mut bot = join(&server, "Toucher").await;
        let mark = mark_id(&bot);
        scarecrow_up(&mut bot, mark).await;

        bot.use_at_nothing().await.expect("send");
        bot.expect_block(USED_ENTITY, mark, PATIENCE)
            .await
            .expect("the mod should hear the use");
        let deadline = tokio::time::Instant::now() + PATIENCE;
        while tokio::time::Instant::now() < deadline
            && !bot
                .notices()
                .iter()
                .any(|text| text.starts_with("nothing selected to build with"))
        {
            let _ = tokio::time::timeout(Duration::from_millis(200), bot.recv()).await;
        }
        assert!(
            bot.notices()
                .iter()
                .any(|text| text.starts_with("nothing selected to build with")),
            "a bare `false` said nothing; notices {:?}",
            bot.notices()
        );
        bot.sleep_ticks(4).await;
        assert!(
            !bot.saw_block(USED_BLOCK, mark),
            "a handled use of an entity went on to the block use"
        );
    });
    assert!(server.stop());
}
