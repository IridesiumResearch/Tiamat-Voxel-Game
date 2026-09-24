<!-- SPDX-FileCopyrightText: Iridesium -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Licensing checklist for a default-mod repository

For the agent that owns one of Iridesium's default-mod repositories — Life,
World, Weather, UI — and for anyone who starts another. Doing every step below
makes your repository licensed identically to the engine and to the other mods,
which is the point: **five repositories, one set of terms.** The engine
repository is the source of truth; a licensing change starts there and is
mirrored out, never the other way round.

**Why now.** Counsel reviewed the engine's terms on 2026-09-24 and found two
gaps. First, a contribution licensed under the bare GPL does not carry the
mod exception, so contribution terms must say the grant is *GPL-3.0-only
together with the Additional Permission*. Second, the engine's exception covers
the engine, not the default mods, and the mods are what other mods actually
build on — UI takes interface elements from other mods, Life's exports are used
by Weather — so each mod needs a permission of its own over its exports. The
decision that follows: **every default mod is GPL-3.0-only with its own
Additional Permission, version 1.0 of 24 September 2026, copyright Iridesium.**

The parameters that differ per mod are three: the mod's display name, its
`mod.toml` identifier, and the path of the file that documents its exports.
Everything else is copied verbatim.

## The steps

1. **`LICENSE`.** Copy the engine's `LICENSE` and change only its first line,
   `Tiamat voxel engine`, to the mod's display name. Put one copy at the
   repository root and a byte-identical copy inside the shipped mod directory,
   `mods/<id>/LICENSE`. The packager refuses a mod directory without one, and
   the root copy is what GitHub shows. (Weather: this replaces the MIT licence;
   see step 10.)

2. **`LICENSE.EXCEPTION`.** Copy the engine's
   `docs/licensing/mod-exception-template.txt`, fill the three placeholders —
   `{{MOD_NAME}}`, `{{MOD_ID}}`, `{{EXPORTS_DOC}}` — and change nothing else.
   Same two places as `LICENSE`. Do not edit the legal text; if it needs
   changing, it changes in the engine's template first, with a new version,
   and every mod re-copies it.

3. **`mod.toml`.** `license = "GPL-3.0-only"`. Not an invented expression
   with the exception in it: the exception is the file beside the manifest.

4. **SPDX headers on every source file** — `.lua`, `.py`, `.rs`, `.sh` — two
   lines, in the file's own comment syntax, before anything else:

   ```lua
   -- SPDX-FileCopyrightText: Iridesium
   -- SPDX-License-Identifier: GPL-3.0-only
   ```

   Both lines. Some repositories carry only the second; the copyright line is
   what standing to enforce rests on. The engine's `api/` files you vendor
   (`stubs/game.lua`, `AGENTS.md`) keep their `MIT` identifier and their
   Iridesium copyright line. A file that is somebody else's — a vendored
   library, a third-party asset — lives under a `third-party/` path component
   with its own licence file beside it and is exempt.

5. **An assets register**, `docs/assets.md` (UI's existing `docs/artwork.md`
   counts): one line per binary file that is not Iridesium's original work —
   its source, its author, its licence, and where that licence is in the tree.
   Fonts under the OFL keep their OFL file beside them. Everything Iridesium
   made — textures drawn, sounds recorded, models built — is GPL-3.0-only like
   the code, and needs no line here. If the register would be empty, say so in
   the README's licence section instead of creating it.

6. **An exports document.** One file that lists what the mod deliberately
   offers other mods: the table it `game.export`s, the hooks and events it
   accepts, the data formats it reads and writes for others, and the
   identifiers it registers. Weather has `docs/exports-contract.md`; the others
   create `docs/exports.md`. The exception's definition of "the Exports" names
   this file, so it must exist and be the path written into
   `LICENSE.EXCEPTION`. Keep it current: it is the line between "using this
   mod" and "copying it".

7. **The README's licence section**, verbatim, name substituted:

   > ## Licence
   >
   > GPL-3.0-only, © Iridesium, with an Additional Permission under GPLv3 §7 in
   > `LICENSE.EXCEPTION` (version 1.0, 24 September 2026): a mod that interacts
   > with <Mod name> only through its exports, the engine's scripting API or
   > the network protocol is an independent work and may be licensed however
   > its author likes. Copying or adapting this mod's code or assets is not
   > covered by that permission and stays under the GPL. `docs/exports.md`
   > lists the exports; the engine's `MOD-LICENSING.md` has the plain-language
   > version and a matrix of what needs which permission. Third-party assets
   > are listed in `docs/assets.md` with their own licences. Contributions are
   > taken under the Developer Certificate of Origin with authors retaining
   > copyright; see `CONTRIBUTING.md`.

   Remove any sentence saying the *engine's* exception makes this mod an
   independent work. It does not; the mod's own does.

8. **`CONTRIBUTING.md`.** Copy the engine's "Licensing and provenance" section
   and its Developer Certificate of Origin text, with the mod's name in place of
   the engine's, `api/` replaced by the vendored `stubs/`, and the grant
   sentence reading:

   > By submitting a contribution to this repository, you license it under the
   > GNU General Public License, version 3 only (`GPL-3.0-only`), together with
   > the Additional Permission in `LICENSE.EXCEPTION`, version 1.0 of 24
   > September 2026. You retain copyright.

   From this commit on, every commit carries a `Signed-off-by` trailer:
   `git commit -s`. Copy the engine's `scripts/check-spdx.sh` (change its
   `api/*` rule to `stubs/*`) and `scripts/check-dco.sh`, and run both in
   whatever the repository already runs as its check.

9. **History.** Each mod repository has one commit authored as "Joel" and the
   rest as Iridesium. If Joel is Iridesium under another git identity, add a
   `.mailmap` line mapping the two and say so in your report. If Joel is
   someone else, that person needs to confirm the grant in step 8 for their
   commit; report it and do not guess.

10. **Weather only.** The repository is MIT today and becomes GPL-3.0-only with
    the exception, which Iridesium as copyright holder may do. Copies already
    distributed under MIT stay MIT; that is fine and needs no mention. Replace
    `LICENSE`, the `mod.toml` field, every `MIT` header outside `stubs/` and
    `AGENTS.md`, and the README.

11. **One commit**, `chore(licence): unify with the engine (exception v1.0)`,
    signed off, and a report back listing: the files added or replaced; the
    header count as "N of N source files carry both lines"; the exports
    document's path; every third-party asset and where its licence is; and
    the answer to step 9.

## What not to do

- Do not edit the template's legal text, and do not write your own exception.
- Do not use an SPDX expression such as `GPL-3.0-only WITH …`; there is no
  registered identifier for this exception.
- Do not remove or move a third-party licence file (an OFL beside a font).
- Do not relicense anything that is not Iridesium's to relicense.
- Do not claim the engine's exception covers the mod, or the mod's covers the
  engine.

## Checking your work

```sh
# every source file carries both lines
for f in $(git ls-files | grep -E '\.(lua|py|rs|sh)$' | grep -v '/third-party/'); do
  grep -q 'SPDX-FileCopyrightText: Iridesium' "$f" && grep -q 'SPDX-License-Identifier: ' "$f" || echo "missing: $f"
done
# the two copies of each licence file are identical
cmp LICENSE mods/*/LICENSE && cmp LICENSE.EXCEPTION mods/*/LICENSE.EXCEPTION && echo "copies match"
# no placeholder survived
grep -n '{{' LICENSE.EXCEPTION && echo "placeholder left" || echo "no placeholders"
# the exports document the exception names exists
grep -o 'documented in [^ ]*' LICENSE.EXCEPTION
```
