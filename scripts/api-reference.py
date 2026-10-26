#!/usr/bin/env python3
# SPDX-FileCopyrightText: Iridesium
# SPDX-License-Identifier: GPL-3.0-only
"""The reference section of api/AGENTS.md, generated from api/stubs/game.lua.

Every function and method the stubs declare, grouped by what it is for, with
the first sentence of its doc comment. The stubs stay the source of truth:
this script writes the section between its markers (`--write`) or fails when
the section in AGENTS.md is not what it would write (`--check`, which
scripts/check-stubs.sh runs in CI). A function the stubs declare that no
area below names fails the check by name, so a new function is placed before
it ships; one an area names that the stubs no longer declare fails the same
way, so a removed function leaves the list.

Usage: scripts/api-reference.py --write | --check
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
STUBS = ROOT / "api" / "stubs" / "game.lua"
AGENTS = ROOT / "api" / "AGENTS.md"
START = "<!-- api-reference:start -->"
END = "<!-- api-reference:end -->"

# What a method is called on, in the stubs' own examples.
RECEIVERS = {
    "ChunkBuffer": "buf",
    "Heightmap": "heightmap",
    "Density": "density",
    "Map": "map",
    "Stream": "stream",
    "Schematic": "schematic",
}

# A doc comment whose first paragraph is about something larger than the
# function it precedes. The stubs are right; the line here is the summary.
SUMMARIES = {
    "game.register_on_dialog_event": (
        "Called when a player acts in a dialog you opened: a button, a slot "
        "click, a text, a shape, a stack dropped outside the sheet."
    ),
}

# The areas, in the order they are written, each with its functions.
AREAS = [
    (
        "Your mod, and what it shares",
        [
            "game.log",
            "game.world_option",
            "game.register_setting",
            "game.setting",
            "game.export",
            "game.exports",
            "game.content_hash",
        ],
    ),
    (
        "Registering blocks, items, fluids and tools",
        [
            "game.register_block",
            "game.register_item",
            "game.register_fluid",
            "game.register_tool",
            "game.get_block_id",
            "game.block_of",
            "game.fluid_id",
            "game.hardness",
            "game.tags",
            "game.tagged",
        ],
    ),
    (
        "Worldgen: generators and callbacks",
        [
            "game.register_on_world_init",
            "game.register_on_generate",
            "game.register_chunk_tint",
            "game.register_chunk_fog",
            "game.noise_heightmap",
            "game.flat_heightmap",
            "game.rng_stream",
            "game.density",
            "game.map",
            "game.schematic",
            "game.schematic_shapes",
        ],
    ),
    (
        "Worldgen: the chunk buffer",
        [
            "ChunkBuffer:fill_all",
            "ChunkBuffer:fill_below_heightmap",
            "ChunkBuffer:fill_density",
            "ChunkBuffer:fill_palette",
            "ChunkBuffer:fill_layers",
            "ChunkBuffer:fill_cover",
            "ChunkBuffer:fill_fluid_below",
            "ChunkBuffer:fill_fluid_terraced",
            "ChunkBuffer:scatter",
            "ChunkBuffer:place",
            "ChunkBuffer:set_block",
            "ChunkBuffer:set_world",
            "ChunkBuffer:set_subnode",
            "ChunkBuffer:set_subnode_world",
            "ChunkBuffer:is_expanded",
        ],
    ),
    (
        "Worldgen: fields, maps, heightmaps and random streams",
        [
            "Density:len",
            "Density:bounds",
            "Density:at",
            "Map:side",
            "Map:scale",
            "Map:noise",
            "Map:offset",
            "Map:scale_by",
            "Map:clamp",
            "Map:blur",
            "Map:combine",
            "Map:fill",
            "Map:heightmap",
            "Heightmap:len",
            "Stream:below",
            "Stream:next_bool",
            "Schematic:len",
        ],
    ),
    (
        "The world at run time",
        [
            "game.get_block",
            "game.set_block",
            "game.get_fluid",
            "game.set_fluid",
            "game.get_light",
            "game.surface_at",
            "game.line_of_sight",
            "game.find_path",
            "game.stamp_schematic",
            "game.time_of_day",
            "game.set_time_of_day",
        ],
    ),
    (
        "Block events: vetoes, handlers and ticks",
        [
            "game.register_on_dig_start",
            "game.register_on_dig_complete",
            "game.register_on_place",
            "game.register_on_punch",
            "game.register_on_use",
            "game.register_on_use_entity",
            "game.register_on_fluid_flow",
            "game.register_random_tick",
            "game.register_on_tick",
        ],
    ),
    (
        "Players",
        [
            "game.register_on_player_join",
            "game.register_on_player_leave",
            "game.register_on_player_move",
            "game.register_on_chat",
            "game.chat_to",
            "game.move_player",
            "game.push_player",
            "game.set_player_abilities",
            "game.set_player_model",
            "game.set_player_skin",
            "game.custom_skins_allowed",
            "game.is_operator",
            "game.player_certified",
            "game.looking_at",
            "game.look_direction",
            "game.heading",
            "game.player_entity",
        ],
    ),
    (
        "Inventories and containers",
        [
            "game.register_view",
            "game.set_main_slots",
            "game.set_slot_grain",
            "game.inventory",
            "game.give",
            "game.take",
            "game.held",
            "game.slot",
            "game.select_slot",
            "game.get_tool",
            "game.set_tool",
            "game.make_container",
            "game.open_container",
            "game.close_container",
            "game.container",
            "game.container_give",
            "game.container_take",
            "game.break_container",
            "game.container_holder",
            "game.containers",
        ],
    ),
    (
        "Screens: dialogs, the HUD, hover lines, fonts and pictures",
        [
            "game.show_dialog",
            "game.update_dialog",
            "game.close_dialog",
            "game.register_on_dialog_event",
            "game.set_hover",
            "game.set_hud",
            "game.register_hud_script",
            "game.register_font",
            "game.register_picture",
            "game.show_over",
        ],
    ),
    (
        "Input",
        [
            "game.register_action",
            "game.register_on_action",
        ],
    ),
    (
        "Entities and riding",
        [
            "game.register_model",
            "game.register_player_model",
            "game.spawn_entity",
            "game.despawn_entity",
            "game.entity",
            "game.set_entity",
            "game.entities_in_radius",
            "game.register_on_entity_step",
            "game.steer_entity",
            "game.transfer_entity",
            "game.mount",
            "game.dismount",
            "game.mounted",
            "game.register_on_dismount",
        ],
    ),
    (
        "Sound and particles",
        [
            "game.register_sound",
            "game.bind_sound",
            "game.cue",
            "game.play_sound",
            "game.play_loop",
            "game.stop_loop",
            "game.emit_particles",
        ],
    ),
    (
        "Sky, weather and stars",
        [
            "game.register_sky",
            "game.set_domain_sky",
            "game.set_sky_modifier",
            "game.register_clouds",
            "game.set_clouds",
            "game.set_precipitation",
            "game.set_rainbow",
            "game.flash",
            "game.lightning",
            "game.stars",
            "game.world_position",
            "game.star_in_view",
        ],
    ),
    (
        "Domains: simulation spaces",
        [
            "game.register_domain",
            "game.create_domain",
            "game.destroy_domain",
            "game.register_on_domain_enter",
            "game.register_on_domain_exit",
        ],
    ),
]


def first_sentence(doc):
    """The first sentence of a doc comment, as one line."""
    text = " ".join(line.strip() for line in doc).strip()
    # A sentence ends at a full stop followed by a space, or at the end;
    # a stop inside backticks (`game.give`) is not one.
    depth = 0
    for index, char in enumerate(text):
        if char == "`":
            depth ^= 1
        elif char == "." and depth == 0:
            nxt = text[index + 1 : index + 2]
            if nxt in ("", " "):
                return text[: index + 1]
    return text


def parse(stubs):
    """Every declared function and the game table's fields, from the stubs."""
    functions = {}
    fields = []
    doc = []
    in_game_class = False
    for line in stubs.read_text(encoding="utf-8").split("\n"):
        if line.startswith("---@class Tiamat.Game"):
            in_game_class = True
            doc = []
            continue
        if in_game_class:
            if line.startswith("---@field "):
                match = re.match(r"---@field (\w+) (\S+) (.*)", line)
                fields.append((match.group(1), match.group(2), first_sentence([match.group(3)])))
                continue
            in_game_class = False
        if line.startswith("---"):
            body = line[3:]
            if body.startswith("@") or body.lstrip().startswith("#"):
                # An annotation, or a heading in a longer doc: not the sentence.
                if body.startswith("@") and doc and doc[-1] != "":
                    doc.append("")
                continue
            doc.append(body)
            continue
        if line.startswith("function "):
            match = re.match(r"function ([A-Za-z_.:]+)\((.*)\) end", line)
            if match is None:
                sys.exit(f"api-reference: a function line the parser does not read: {line}")
            paragraph = []
            for body in doc:
                if body.strip() == "":
                    if paragraph:
                        break
                    continue
                paragraph.append(body)
            functions[match.group(1)] = (match.group(2), first_sentence(paragraph))
            doc = []
            continue
        doc = []
    return functions, fields


def display(name, args):
    if ":" in name:
        cls, method = name.split(":", 1)
        return f"{RECEIVERS[cls]}:{method}({args})"
    return f"{name}({args})"


def render(functions, fields):
    """The section's text, between the markers."""
    out = [START, ""]
    out.append("### The `game` table's fields")
    out.append("")
    for name, kind, summary in fields:
        out.append(f"- `game.{name}` ({kind}) — {summary}")
    out.append("")
    for title, names in AREAS:
        out.append(f"### {title}")
        out.append("")
        for name in names:
            args, summary = functions[name]
            summary = SUMMARIES.get(name, summary)
            out.append(f"- `{display(name, args)}` — {summary}")
        out.append("")
    out.append(END)
    return "\n".join(out)


def main():
    if len(sys.argv) != 2 or sys.argv[1] not in ("--write", "--check"):
        sys.exit(__doc__)
    functions, fields = parse(STUBS)
    named = [name for _, names in AREAS for name in names]
    duplicates = sorted({name for name in named if named.count(name) > 1})
    unplaced = sorted(set(functions) - set(named))
    gone = sorted(set(named) - set(functions))
    problems = []
    if duplicates:
        problems.append(f"named in two areas: {', '.join(duplicates)}")
    if unplaced:
        problems.append(
            "declared in the stubs but in no area of scripts/api-reference.py "
            f"(add each to the area it belongs to): {', '.join(unplaced)}"
        )
    if gone:
        problems.append(
            "named in scripts/api-reference.py but no longer in the stubs "
            f"(remove each from its area): {', '.join(gone)}"
        )
    if problems:
        for problem in problems:
            print(f"API REFERENCE: {problem}")
        sys.exit(1)
    section = render(functions, fields)
    text = AGENTS.read_text(encoding="utf-8")
    start, end = text.find(START), text.find(END)
    if start < 0 or end < 0 or end < start:
        sys.exit(f"api-reference: {AGENTS} has no '{START}' … '{END}' section to fill")
    end += len(END)
    current = text[start:end]
    if sys.argv[1] == "--write":
        if current != section:
            AGENTS.write_text(text[:start] + section + text[end:], encoding="utf-8")
            print(f"api-reference: wrote {len(functions)} entries and {len(fields)} fields")
        else:
            print("api-reference: up to date")
        return
    if current != section:
        print(f"API REFERENCE OUT OF DATE in {AGENTS.relative_to(ROOT)}:")
        have, want = set(current.split("\n")), set(section.split("\n"))
        for line in section.split("\n"):
            if line not in have:
                print(f"  + {line}")
        for line in current.split("\n"):
            if line not in want:
                print(f"  - {line}")
        print("  Run scripts/api-reference.py --write and commit the result.")
        sys.exit(1)
    print(f"API reference: OK ({len(functions)} entries, {len(fields)} fields)")


if __name__ == "__main__":
    main()
