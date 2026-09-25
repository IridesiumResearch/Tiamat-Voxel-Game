<!-- SPDX-FileCopyrightText: Iridesium -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Distribution and updates

**Authoritative.** How a built Tiamat reaches a player and how it keeps itself
current. Any change to the manifest format, the trust rules, or the install
layout is edited here first.

Decided 2026-09-21, for the first round of outside testing: a launcher applies
updates, builds are unsigned for now, GitHub Releases holds the artefacts and
the website mirrors them, and the targets are `x86_64-pc-windows-msvc`,
`aarch64-apple-darwin`, `x86_64-apple-darwin` and `x86_64-unknown-linux-gnu`.
Where the manifest is served from, and how, is [`hosting.md`](hosting.md).

---

## 1. The shape: who downloads, who applies

Three parts, and the split is the whole design:

| Part | Job | Changes |
|---|---|---|
| **Launcher** | Applies a staged update, then starts the game. No network, no UI. | Rarely |
| **Client** | Checks the manifest, downloads an update, verifies and stages it. | Every release |
| **Manifest** | Says what the current version is and how to verify it. Signed. | Every release |

**The client downloads and the launcher applies**, rather than either doing
both. The client already has a window, a front screen and a progress idiom, so
a 150 MB download can show what it is doing; the launcher runs for a fraction
of a second with nothing to report. And nothing ever overwrites a running
binary: the client stages an update and exits, the launcher swaps the files
before anything is open.

The player's shortcut points at the launcher. The launcher is what a release
almost never has to replace, which matters because **a launcher that breaks
cannot fix itself** — it is the one component whose failure needs a human and a
fresh download.

## 2. Trust: charter rule 14 applied to our own bytes

An updater executes what it downloads. It is the most dangerous code in the
project, and the rules are not negotiable:

- **The trusted key lives in the repository**, at `release-key.pub`, and is
  compiled in from there. Not a CI variable: a variable can be changed by
  anyone with settings access and leaves no trace, while a change to the file
  is a diff in the history. A build that finds no key applies no updates at
  all, which is what a fork with no key of its own should do.
- **The signature is the trust, not the transport.** The manifest is fetched
  over HTTPS with the pure-Rust TLS stack already in the tree, and then its
  Ed25519 signature is checked against a public key **compiled into the binary**.
  A mirror, a CDN, a compromised host and a mis-issued certificate are all
  therefore harmless: they can refuse to serve, and they cannot serve something
  else. This is why §1 can say the website mirrors GitHub without the mirror
  being trusted.
- **Every artefact is named by its BLAKE3 hash in the signed manifest**, and
  the hash is checked before a single byte is unpacked. The same function the
  content cache already uses.
- **Caps before allocation.** The manifest has a maximum size, a maximum
  artefact count and a maximum artefact size, all checked before anything is
  read into memory.
- **An archive is a hostile parser.** Path traversal (`../`), absolute paths,
  symlinks pointing out of the tree, and archive bombs are all refused by
  construction rather than by inspection, and the extractor ships with a fuzz
  target in the same change it lands in.
- **No downgrade.** A manifest whose version is older than what is installed is
  refused, so a captured old manifest cannot be replayed to push a player back
  onto a version with a known hole.
- **Per-user install, never an administrator.** Nothing asks for elevation,
  which means a compromised update cannot become a compromised machine.
- **The key can be rotated, and the rotation is pre-committed.** Each manifest
  carries the BLAKE3 hash of the NEXT signing key. A client that has seen
  version *n* will accept a key at *n+1* only if it matches the hash it was
  already told, which is charter rule 13's model for player identity, used
  again for the thing that ships code.

## 3. The manifest

One file per channel, served at a stable URL, small enough to read in a
breath. Signed as a detached Ed25519 signature over the exact bytes.

```json
{
  "schema": 1,
  "channel": "test",
  "version": "0.2.0",
  "commit": "8929ca1…",
  "released": "2026-09-21T10:00:00Z",
  "protocol": 70,
  "next_key": "b3:…",
  "notes": "https://…/releases/0.2.0",
  "artifacts": [
    {
      "target": "aarch64-apple-darwin",
      "name": "tiamat-0.2.0-aarch64-apple-darwin.tar.gz",
      "size": 148223118,
      "hash": "b3:…",
      "urls": ["https://github.com/…", "https://…mirror…"]
    }
  ]
}
```

- **`protocol`** is `proto::PROTOCOL_VERSION`. A client refuses a server whose
  protocol differs, so during a test round the download page can say which
  server build a version talks to, and the client can say "this server wants
  version 0.2.1" rather than "connection refused".
- **`commit`** is the source the binaries were built from. It is what makes the
  GPL's corresponding-source offer exact rather than approximate (§6).
- **`urls`** are tried in order. Both are untrusted; the hash decides.

## 4. Versions and channels

Two channels: **`test`** while there are testers, **`stable`** when there is a
public build. A channel is a separate manifest URL, and a client is built for
one — there is no switching at runtime, because a build that could switch could
be talked into switching.

The version is the workspace version plus the release's commit, stamped into
the binary at build time. `0.2.0`, tagged 2026-09-25, is the first testing
build; `0.1.0` was the repository before any release.

## 5. Install layout

Per user, no elevation, and the same shape everywhere so one set of
instructions works:

```
<install>/
  launcher            the shortcut points here
  current/            the game: client, server, game/ mods, licences
  staged/             an update that has been verified and not yet applied
  previous/           what `current` was, kept for one rollback
  install.json        installed version, channel, and where it came from
```

Windows `%LOCALAPPDATA%\Tiamat`, macOS `~/Library/Application Support/Tiamat`
(inside a `Tiamat.app` bundle for the Finder's sake), Linux
`~/.local/share/tiamat`. Saves, the identity key and the content cache stay
where they already live and are **never** touched by an update — a player who
reinstalls keeps their worlds.

**Applying an update is a rename, not a copy**: `current` → `previous`,
`staged` → `current`, on the same filesystem, so a power cut leaves one of the
two intact rather than a half-written tree. If the game fails to start twice in
a row, the launcher puts `previous` back.

## 6. Mods

The engine ships with the reference mods in `game/`, which is what makes
singleplayer work out of the box. A player joining someone else's server needs
none of them: textures, sounds, models and HUD scripts are pushed by hash by
the server they join.

The mods **in** this repository carry the repository's licence and its SPDX
headers, and need nothing else. A mod from **outside** it — the default world,
life, weather and UI mods live in their own repositories, and appear here as
symlinks for development — is bundled **at the commit `bundle.toml` pins**,
never from a working tree. `scripts/bundle-lock.sh` writes that file from the
repositories beside the checkout (refusing a commit nobody has pushed), it is
committed with the version bump before a release is tagged, and the packager
takes each mod from the checkout beside it when that has the commit and from
the repository otherwise, so CI and a maintainer's machine build the same
archive. A bundled mod is only taken if it carries its own `LICENSE` inside
its mod directory (and its own `LICENSE.EXCEPTION`, warned about if absent —
see `docs/licensing/mod-repo-checklist.md`); the packager refuses one that
does not, loudly, because an archive quietly missing the notice for the mod
that makes the game a game is worse than no archive. `--allow-missing-mods`,
for a bare test build, leaves such a mod out rather than shipping it without
its notice: the mod goes, not the rule.

## 7. Licences, and why this section is not optional

Distributing binaries of a GPLv3 program carries obligations that a website
download makes real:

- **The corresponding source must be available.** The repository is public and
  the manifest names the exact commit; the download page links both, and
  `LICENSE` ships inside every archive.
- **`LICENSE.EXCEPTION` ships too**, because the Additional Permission under
  §7 is what tells a mod author their work is not derivative.
- **Third-party notices, as notices.** `cargo deny` gates which licences may
  enter the tree, but a list of names and SPDX identifiers preserves nobody's
  notice, and MIT, BSD and Apache-2.0 each require theirs preserved in every
  copy. So every archive carries `licenses/<crate>-<version>/` for every crate
  compiled into its binaries, holding that crate's own licence and notice files
  copied from the published package; where a package ships none, the canonical
  text of each licence it names (from `scripts/licenses/`) and its authors as
  copyright holders, and the index says so. `THIRD-PARTY.md` is the index. The
  embedded font's licence is in `licenses/go-font/`. Each bundled mod's notices
  are inside its own directory. `scripts/third-party-notices.py` produces all of
  it, per target, since the dependency set differs by platform.
- **The release record.** `RELEASE.md` in every archive names the engine commit
  and every bundled mod's repository and commit, says where the corresponding
  source is, and lists the licence files; `MANIFEST.txt` lists every file in
  the archive with its SHA-256. A moving `main` is not a reference for an
  older binary; these are.
- **The corresponding source ships beside the binaries.** The release workflow
  publishes `tiamat-<version>-source.tar.gz`: the engine at the tag and every
  bundled mod's repository at its pinned commit, with a `SOURCE.md` that says
  how to build. That is the GPL's offer made good on the same page the
  binaries are downloaded from, not a promise that a repository will still be
  there. `relman` leaves it out of the update manifest, since no client fetches
  it.
- **Checked by opening the archive, not by trusting the configuration.**
  `scripts/check-archive.sh` unpacks a built archive and verifies every item
  above — the licence files, the notice directories against the index, each
  bundled mod's licence and commit, the record, the manifest, no `.git` and no
  symlinks. The release workflow runs it on every archive it produces.

## 8. Releasing

1. Run `scripts/bundle-lock.sh` and commit `bundle.toml` with the version
   bump, so the tag records which commit of each default mod it carries. Then
   tag the commit. CI builds all four targets, packages them, checks each
   archive, builds the source archive, and uploads all of it to a **draft**
   GitHub Release.
2. **Sign locally.** `cargo run -p relman -- sign` reads the draft's hashes,
   writes the manifest and signs it with the release key, which lives on the
   maintainer's machine and **never** in CI. A private key in a CI secret is a
   private key in everybody's pull request workflow.
3. Publish the release, then publish the manifest and its signature at the
   update host. Where that is, how it is run, and the release steps with their
   exact commands are in [`hosting.md`](hosting.md).
4. Older manifests stay published: they are the record of what was signed, and
   a client that has been offline for three versions verifies against the
   current one regardless.

## 9. Deliberately not done yet

Recorded so a later reader knows these were decided rather than forgotten:

- **Code signing and notarisation.** Unsigned for this round (decided
  2026-09-21). macOS testers clear the quarantine flag once; Windows shows a
  SmartScreen warning. **Our own signature check is unaffected** — it does not
  depend on the operating system's opinion of the binary. Revisit before any
  public release.
- **Delta updates.** A full archive per release, which is tens of megabytes a
  tester downloads each time. Worth doing when releases outpace patience.
- **A launcher with a window.** The launcher reports through the client's own
  front screen, and shows nothing itself. If an update ever has to be applied
  slowly enough to notice, it needs one.
- **An in-place installer for Windows** (MSI or Inno Setup). The first round
  ships an archive and a launcher that installs on first run.
