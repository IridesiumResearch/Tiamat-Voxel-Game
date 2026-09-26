-- SPDX-FileCopyrightText: Iridesium
-- SPDX-License-Identifier: MIT
--
-- {{MOD_NAME}}: a tour of the Tiamat mod API, one thing of each kind.
--
-- Everything a mod can do goes through `game.*`, and every function is
-- documented in `stubs/game.lua` beside this file; point your editor at it
-- (README.md says how) and you get completion and the reason behind each
-- field. What is registered here is registered at load, once, and the window
-- closes after this file returns (charter rule 9); the hooks run for ever.
--
-- Ids are namespaced by the `id` in mod.toml: `beacon` below is
-- `{{MOD_ID}}:beacon` to every other mod, and `game.mod_id` is that id at
-- run time. Delete what you do not need; a mod that registers one block is a
-- complete mod.

-- A BLOCK. Every face wears `textures/block.png`, a 16 by 16 PNG in this
-- directory, and it glows a little: `light_emit` is 0..15 per channel.
-- Without a texture a block reaches players as the missing-texture chequer,
-- so this is the one field that is not really optional.
local beacon = game.register_block{
    id = "beacon",
    name = "Beacon",
    description = "A block this mod brought. Dig it, place it, use it.",
    textures = { all = "textures/block.png" },
    light_emit = { r = 6, g = 14, b = 15 },
}

-- A TOOL. The engine has no bare hand of its own: a world whose mods
-- register no `default` tool is one nobody can dig in. `brush = "block"`
-- takes whole blocks; `"subnode"` takes one cell of the 27, which is how a
-- chisel works. Leave this out if another mod in your set already provides
-- a hand.
game.register_tool{
    id = "hand",
    brush = "block",
    speed_multiplier = 1.0,
    default = true,
}

-- A SOUND, from a WAV or Ogg file in this directory. It is played where
-- something happens, never at the listener, so somebody standing nearby hears
-- it from the right side.
game.register_sound{ id = "ping", file = "sounds/ping.wav", gain = 0.8, pitch_variance = 0.1 }

-- An ACTION. Mods name actions and the engine owns the key bindings (charter
-- rule 11): `default_key` is a suggestion the player can move on the controls
-- screen, and this mod never reads a key.
game.register_action{
    id = "wave",
    default_key = "KeyJ",
    description = "Wave: open this mod's dialog",
}

-- A DIALOG, when the action is pressed. A dialog is a tree of widgets the
-- client draws; what the player does with it comes back as events, below.
-- `event.id` is the qualified action id, so this mod's own is compared with
-- `game.mod_id` in front.
game.register_on_action(function(event)
    if event.id ~= game.mod_id .. ":wave" or not event.pressed then
        return
    end
    game.show_dialog{
        player = event.player,
        form = "wave",
        tree = {
            type = "container", direction = "column", gap = 8, padding = 12, children = {
                { type = "label", text = "Hello from " .. game.mod_id },
                { type = "button", name = "mark", text = "Drop a beacon at my feet" },
                { type = "button", name = "done", text = "Close" },
            },
        },
    }
end)

-- An ENTITY, when the button is pressed. An entity is a thing in the world
-- that is not a block: a creature with a model, or — as here — a stack lying
-- on the ground, which any pickup rule you write can find with
-- `game.entities_in_radius`. A collider makes it fall and stand; without one it
-- would hang in the air as a marker.
--
-- The event names the form as it went to the client, with this mod's id in
-- front, the way `event.id` names an action.
game.register_on_dialog_event(function(event)
    if event.form ~= game.mod_id .. ":wave" or event.kind ~= "pressed" then
        return
    end
    if event.name == "mark" then
        local id = game.player_entity(event.player)
        local body = id and game.entity(id)
        if body then
            game.spawn_entity{
                pos = { x = body.pos.x, y = body.pos.y + 1, z = body.pos.z },
                item = { material = beacon, count = 1 },
                collider = { width = 0.5, height = 0.5 },
            }
            game.play_sound{ sound = "ping", pos = body.pos }
        end
    end
    game.close_dialog{ player = event.player, form = "wave" }
end)

-- USING the block: the place control on it with nothing to place. Returning
-- "" says this mod handled it and the player is told nothing more; returning
-- nothing lets the next mod, and then the engine, answer.
game.register_on_use(function(event)
    if event.material ~= beacon then
        return
    end
    game.chat_to(event.player, "the beacon hums")
    game.play_sound{ sound = "ping", pos = { x = event.x / 3, y = event.y / 3, z = event.z / 3 } }
    return ""
end)

-- A word on join, to the one player. `game.log` goes to the server's log;
-- `game.chat_to` goes to a person.
game.register_on_player_join(function(event)
    game.chat_to(event.player, "{{MOD_NAME}} is loaded: press the wave key, or place a beacon.")
end)

-- Terrain is a mod's too — `game.register_on_generate` and the fills on the
-- chunk buffer it is handed — but this mod leaves it to whichever world mod
-- it is loaded beside, so it can be dropped into any set.
