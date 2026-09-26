-- SPDX-FileCopyrightText: Iridesium
-- SPDX-License-Identifier: GPL-3.0-only
--
-- A reference chest: an inventory that belongs to the WORLD.
--
-- This is a TEST FIXTURE, not the shipped game (see game/README.md). It is
-- the smallest thing that proves the container mechanism works through the
-- public mod API from end to end, and every line of it is the shape a real
-- chest mod would take:
--
--   1. `game.make_container` when the block is placed: one box, at one place,
--      named by where it is, which is how it is found again.
--   2. `game.open_container` and a dialog with an `item_grid` naming it when
--      the block is used. The engine lends the box into the opener's own
--      slots, so clicks move stacks exactly as they do in a backpack, and
--      nothing here moves one.
--   3. `game.container_holder` and `game.break_container` when the block is
--      dug: a chest somebody ELSE has open refuses the dig and says so;
--      otherwise its contents come back and go to the digger (charter rule
--      5: nothing is destroyed).
--
-- What the engine promises about a container — one holder at a time, the
-- contents surviving a disconnect and a restart — is held to it by
-- `crates/bot/tests/containers.rs`; `crates/bot/tests/chest.rs` holds this
-- mod to the three steps above.

--- Slots in a chest: three rows of nine.
local SLOTS = 27
--- The screen's name, so it can be closed by name.
local FORM = "core_chest:chest"

local chest = game.register_block{
    id = "chest",
    name = "Chest",
    description = "A box in the ground. Use it to open it; dig it to take it away, contents and all.",
    textures = { all = "textures/chest.png" },
}

--- The container at a block, named by where it is.
---
--- **Prefixed by this mod on purpose.** Container names are not namespaced by
--- the engine, so that a hopper may feed a furnace by the furnace's own name;
--- the prefix is what keeps another mod's box at the same place a different
--- box.
local function name_at(x, y, z)
    return "core_chest:at:" .. x .. "," .. y .. "," .. z
end

-- 1. Made when placed.
--
-- `make_container` is make-if-absent, and the use below calls it too, so a
-- chest that arrived by some other road — a generator, a stamped plan, a
-- test's seed — still has a box behind it the first time it is opened. This
-- is the tidy path, not the only one.
game.register_on_place(function(event)
    if event.material == chest then
        game.make_container(name_at(event.x, event.y, event.z), SLOTS)
    end
end)

-- 2. Opened with the place control.
--
-- A use names a CELL; the block is the cell over three. The box is lent to
-- this player alone, and `false` from `open_container` means somebody else
-- has it — the one refusal worth saying out loud.
game.register_on_use(function(event)
    if event.material ~= chest then
        return
    end
    local name = name_at(event.x // 3, event.y // 3, event.z // 3)
    game.make_container(name, SLOTS)
    if not game.open_container(name, event.player) then
        return "somebody is using that"
    end
    game.show_dialog{
        player = event.player,
        form = FORM,
        tree = {
            type = "container", direction = "column", gap = 8, children = {
                { type = "label", text = "Chest" },
                { type = "item_grid", view = name, columns = 9, first = 1, count = SLOTS },
                { type = "label", text = "Yours" },
                { type = "item_grid", view = "player:main", columns = 9, first = 1, count = 27 },
            },
        },
    }
    return ""
end)

-- 3. Dug.
--
-- `break_container` answers an empty list for an empty box AND for one it
-- refused to touch because somebody has it open, so the holder is asked
-- first: another player's open chest refuses the dig, and the digger's own is
-- shut for them before it is broken. What was inside goes to the digger. A
-- stack that cannot be given — they left in the same tick — is dropped where
-- the chest stood rather than lost.
game.register_on_dig_complete(function(event)
    if event.material ~= chest then
        return
    end
    local x, y, z = event.x // 3, event.y // 3, event.z // 3
    local name = name_at(x, y, z)
    local holder = game.container_holder(name)
    if holder ~= nil and holder ~= event.player then
        return "somebody is using that"
    end
    if holder ~= nil then
        game.close_dialog{ player = holder, form = FORM }
        game.close_container(name, holder)
    end
    for _, stack in ipairs(game.break_container(name)) do
        -- Units, not `count`: a stack is whole blocks and loose nodes, and
        -- `count` would round the nodes away (charter rule 5).
        local spec = { material = stack.material, units = stack.units, shape = stack.shape, detail = stack.detail }
        if not game.give(event.player, spec) then
            game.spawn_entity{
                pos = { x = x + 0.5, y = y + 0.5, z = z + 0.5 },
                item = spec,
                collider = { width = 0.5, height = 0.5 },
            }
        end
    end
end)
