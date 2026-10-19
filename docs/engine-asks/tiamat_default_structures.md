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

Nothing open as of 2026-10-19. The repository was scaffolded that day from
`server --create-mod` in the siblings' layout, with the API vendored at
ad981ac2 and the conformance workflow in place (milestone 0.5.0 items A3
and A4); its brief is still to be written, so the mod is the template's
tour until then.
