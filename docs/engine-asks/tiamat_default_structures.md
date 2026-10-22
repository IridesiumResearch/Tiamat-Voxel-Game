<!-- SPDX-FileCopyrightText: Iridesium -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Engine asks from Tiamat Default Structures

From the `tiamat_default_structures` mod (the built structures of the
default game; its own repository, beside the engine). Kept here rather than
in that repo so the engine agent finds every mod's asks in one place. What
it asks of the OTHER mods (Progress, Craft, Life, the world, the interface
and the weather) is not the engine's and lives in that repo's
`docs/sibling-asks.md`.

Numbered as in that repo, which keeps the history. Each entry says what was
seen, why the mod cannot fix it, and the smallest engine change that would.
Newest first. Items are removed when they land.

## ST-E2. A chunk buffer handed to another mod's export (2026-10-22): ANSWERED 2026-10-22

**Asked.** Whether the `ChunkBuffer` a generator was handed still works when
passed into another mod's exported function, in a generation worker, and is
written there — so a path mod's generator can call `structures.generate(buf,
pos, …)` and get the same sites as the overworld.

**Answer: yes, and now the stubs say so.** The export boundary
(`crossing` in `crates/core/src/script/mlua_vm.rs`) turns tables into
read-only views and functions into boundary calls, and passes everything else
through unchanged — a `ChunkBuffer` userdata arrives as itself, with every
method the generator has, writable. Generation workers have `game.exports`
like every VM (it is installed with each mod at load), and the dependency rule
is the usual one: the caller lists Structures in `depends` or
`optional_depends`, which also fixes the load order the other way round from
what a Structures-reads-the-paths design would need. **The one rule, now in
the `game.exports` stub:** a buffer is live for the duration of the generator
call it was handed to and no longer — the engine copies the chunk out when the
generators return, and a buffer kept past that is written into nothing,
without an error. Do the work inside the call; never keep a buffer in a table.

## ST-E1. Stamp one schematic at a chosen root, at generation (2026-10-22): LANDED 2026-10-22

**Asked.** `buf:place(schematic, root, { turn, sink })`: one schematic with
its root at a world block position the mod chose, turned 0..3 quarter turns as
`game.stamp_schematic` turns, clipped to the chunk, as a merge write, answering
whether any block landed in this chunk.

**Landed as asked, without `sink`.** `ChunkBuffer::place` in
`crates/core/src/detgen/buffer.rs` shares one write with `scatter`
(`stamp_blocks`, the loop the ask pointed at, factored out), applies
`Schematic::turned(quarters)`, rejects a root whose structure cannot reach the
chunk before touching a block, and saturates offsets at the edge of the
coordinates. Bound as `buf:place(schematic, root, { turn })` with
`stamp_schematic`'s rules for the root and the turn; the stub and the API
reference carry it. `sink` is left out: it is `scatter`'s word for "how far
below the surface it found", and a caller that chose the root has already
chosen that. A structure reaching into several chunks is written by every
chunk it reaches with the same root and turn, as the stub says. Tests: the
root inside the chunk, the clip across an edge and a root that reaches
nothing, the turn against `StampBlock::turned` and four quarters being none,
and the same column by `scatter` and by `place` producing the same chunk.

Nothing else open as of 2026-10-22. The repository was scaffolded that day from
`server --create-mod` in the siblings' layout, with the API vendored at
ad981ac2 and the conformance workflow in place (milestone 0.5.0 items A3
and A4); its brief is still to be written, so the mod is the template's
tour until then.
