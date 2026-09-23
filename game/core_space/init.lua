-- SPDX-FileCopyrightText: Iridesium
-- SPDX-License-Identifier: GPL-3.0-only
--
-- The reference space: every star in the sky is somewhere a player can go.
--
-- **This file is Task 15c, as the smallest thing that proves it.** The prompt
-- asked for a demonstration mod and listed the engine capabilities it would
-- need; those four are what this exercises, and nothing else:
--
--   * `game.stars()` — the catalog the sky is drawn from, one derivation on
--     both ends of the wire, so the star a player looks at is the star this
--     file sends them to;
--   * `game.star_in_view(player)` — which star a player is facing, from where
--     their domain sits, at the hour it is;
--   * `register_domain{ position }` and `create_domain(..., { position })` —
--     a body made AT a star, so the sky drawn from it is that star's, and the
--     place survives a restart with the instance;
--   * `register_sky{ domain }` with `stars` on its keyframes — a sky of a
--     body's own, sent when a player arrives, with the catalog in it.
--
-- What this does NOT do, on purpose: no ship, no thrust, no approach radius, no
-- worldgen from a star's colour. Those are content, and a space mod's to
-- write. This is a chat command and a plain floor, which is exactly enough to
-- assert against.
--
-- Note the shape of the travel rule: it lives here, in Lua, and the engine
-- knows nothing of "visit". Charter rule 1.

local white = game.get_block_id("core:white")

-- A body: a flat white floor, made once per visited star and never for a
-- star nobody has been to. `game.create_domain` makes them at runtime because
-- the registration window could not have named two thousand of them.
game.register_domain{
    id = "body",
    instanced = true,
    generator = function(buf, pos)
        buf:fill_below_heightmap(game.flat_heightmap(0), white)
    end,
}

-- The sky from a body: no dawn, no sun, the whole catalog at full. Named for
-- the template, so every body inherits it. The day length is the world's
-- whatever is written here — one clock — and is required all the same.
game.register_sky{
    domain = "core_space:body",
    day_length_ticks = 24000,
    keyframes = {
        { time = 0.0, sky = {0.0, 0.0, 0.0}, sun = {0.0, 0.0, 0.0}, intensity = 0.0, stars = 1.0 },
    },
}

-- Stars by id, read once. Two thousand tables is nothing to keep and a lot to
-- rebuild on every command.
local stars_by_id = nil
local function star(id)
    if stars_by_id == nil then
        stars_by_id = {}
        for _, record in ipairs(game.stars()) do
            stars_by_id[record.id] = record
        end
    end
    return stars_by_id[id]
end

-- The travel rule. "visit" goes to the star under the crosshair, if one is
-- close enough to the centre of the view that a person would call it looked
-- at; "visit <id>" goes to that star; "home" comes back.
game.register_on_chat(function(event)
    local body = game.player_entity(event.player)
    if body == nil then
        return
    end
    if event.text == "home" then
        game.transfer_entity(body, "overworld", { x = 0, y = 80, z = 0 })
        return false
    end
    local verb, id = event.text:match("^(visit)%s*(%d*)$")
    if verb == nil then
        return
    end
    if id == "" then
        local seen = game.star_in_view(event.player)
        -- Within about a degree: the cosine of the angle between the gaze
        -- and the star, and a player is not a telescope.
        if seen == nil or seen.alignment < 0.9998 then
            game.log(event.player .. " is not looking at a star")
            return false
        end
        id = seen.id
    end
    local target = star(tonumber(id))
    if target == nil then
        game.log("no star " .. tostring(id))
        return false
    end
    -- Made AT the star, so the sky from its surface is the sky seen from
    -- there — and that star is the one not in it.
    local domain = game.create_domain("core_space:body", tostring(target.id), {
        position = { x = target.x, y = target.y, z = target.z },
    })
    if domain == nil then
        game.log("could not make a body for star " .. tostring(target.id))
        return false
    end
    game.transfer_entity(body, domain, { x = 8, y = 4, z = 8 })
    return false
end)

game.log("registered core_space:body; say `visit` at a star, `home` to return")
