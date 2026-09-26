-- SPDX-FileCopyrightText: Iridesium
-- SPDX-License-Identifier: GPL-3.0-only
--
-- A reference copy-and-paste tool: `game.plans`, a box of blocks captured
-- from the world and built again elsewhere.
--
-- This is a TEST FIXTURE, not the shipped game (see game/README.md). It is
-- the smallest thing that proves the plans mechanism works through the public
-- mod API from where a player stands, which here is a chat line: the same
-- calls a build tool would make from a key or a dialog, without inventing
-- either.
--
--   capture <name> <x1> <y1> <z1> <x2> <y2> <z2>   a box of the world, by name
--   stamp <name> <x> <y> <z>                         built again, its corner there
--   plan <name>                                      what a plan holds
--   plans                                            every plan this mod has
--   forget <name>                                    gone, and gone after a restart
--
-- Every answer comes back as a chat line to the player who asked. The words
-- are kept from the room (`return false`): a chat hook is a veto, and a line
-- that is a command is not a thing to say to everybody.
--
-- `crates/bot/tests/core_plans.rs` holds this mod to the words above; what
-- the engine promises about a plan — the size caps, the paced stamp, the
-- survival of a restart — is held to it by `crates/bot/tests/plans.rs`.

--- Three words as a position, or nil if any of them is not a whole number.
local function position(a, b, c)
    local x = math.tointeger(tonumber(a))
    local y = math.tointeger(tonumber(b))
    local z = math.tointeger(tonumber(c))
    if x == nil or y == nil or z == nil then
        return nil
    end
    return { x = x, y = y, z = z }
end

--- A plan's summary as one line: its size, its blocks, and its units by
--- material, in a fixed order so two readings of one plan say the same thing.
local function describe(name, made)
    local parts = {}
    for material, units in pairs(made.materials) do
        parts[#parts + 1] = material .. "=" .. units
    end
    table.sort(parts)
    return string.format("%s: %dx%dx%d, %d blocks, %s",
        name, made.size.x, made.size.y, made.size.z, made.blocks, table.concat(parts, " "))
end

game.register_on_chat(function(event)
    local words = {}
    for word in string.gmatch(event.text, "%S+") do
        words[#words + 1] = word
    end
    local verb, name = words[1], words[2]
    local say = function(text)
        game.chat_to(event.player, text)
    end

    if verb == "capture" then
        local from = position(words[3], words[4], words[5])
        local to = position(words[6], words[7], words[8])
        if name == nil or from == nil or to == nil then
            say("capture <name> <x1> <y1> <z1> <x2> <y2> <z2>")
            return false
        end
        local made, reason = game.plans.capture(name, from, to)
        if made == nil then
            say("cannot capture " .. name .. ": " .. tostring(reason))
        else
            say("captured " .. describe(name, made))
        end
        return false
    elseif verb == "stamp" then
        local at = position(words[3], words[4], words[5])
        if name == nil or at == nil then
            say("stamp <name> <x> <y> <z>")
            return false
        end
        if game.plans.stamp(name, at) then
            say("stamping " .. name .. " at " .. at.x .. "," .. at.y .. "," .. at.z)
        else
            say("cannot stamp " .. name .. ": no such plan, or too many waiting")
        end
        return false
    elseif verb == "plan" and name ~= nil then
        local info = game.plans.info(name)
        say(info and describe(name, info) or ("no plan called " .. name))
        return false
    elseif verb == "plans" then
        local names = game.plans.list()
        say(#names == 0 and "no plans" or ("plans: " .. table.concat(names, " ")))
        return false
    elseif verb == "forget" and name ~= nil then
        say((game.plans.forget(name) and "forgot " or "no plan called ") .. name)
        return false
    end
end)
