<!-- SPDX-FileCopyrightText: Iridesium -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Implementing the accounts system — a brief for a coding assistant

*2026-10-21. For an AI coding assistant working in this repository (the first
is DeepSeek, driven by the designer) and the person supervising it. The
design is decided and is not re-opened here; this is the work order, the
reading list, and the house rules. The engine's own agent reviews, gates and
merges what lands, so work on a branch and never push to `main`.*

## Read first, in this order

1. `CLAUDE.md` — the charter. Rules 13 (identity), 14 (hostile input) and 17
   (licensing) govern this work; "Style" governs every line.
2. `CONTRIBUTING.md` — the sign-off every commit needs, the house rules, and
   "Changing the wire protocol" (for N3).
3. `docs/accounts-design.md` — the design. §2 is the certificate, §3 the
   name rules, §4 the flows, §5 the server, §6 the client, §7 the service,
   §8 what is kept. Where the code and the design disagree, fix the design
   in the same commit and say so.
4. The code to mirror, so the new code reads like the old:
   - `crates/core/src/identity/mod.rs` — `Identity`, `PlayerUuid`, and
     `challenge_payload` / `verify_challenge`: every signed payload here is
     domain-tagged with a `tiamat:<purpose>:v1` prefix, and verification uses
     `verify_strict`.
   - `crates/core/src/identity/keyset.rs` — `commit_to`, the BLAKE3
     commitment a key makes to its successor, and the rotation payloads.
   - `crates/core/src/release.rs` — `Manifest::parse` checks every cap before
     decoding; `Manifest::verify` accepts the trusted key or the successor it
     committed to, and nothing else. The certificate follows the same shape.
   - `crates/core/src/proto/mod.rs` — `WireSignature` (a 64-byte signature
     under serde, exactly 64 or refused), `MAX_NAME_BYTES`, and `Hello`.
   - `fuzz/fuzz_targets/manifest_decode.rs` and `fuzz/Cargo.toml` — what a
     fuzz target looks like and how it is registered.

Do not read the whole repository; the files above are enough for N2. The
mod API (`api/`) is not touched until N3.

## The work, in the milestone's order

The rows are in `docs/milestone-0.5.0.md`: **N2** the certificate in core,
**N5** the service as a library with a binary, **N3** the protocol and the
server, **N4** the client, **N6** the bot tests. N2 then N5 first, so N6's
tests can run against a real registry started in-process.

### N2 — `crates/core/src/identity/registry.rs`

Declared as `pub mod registry;` in `identity/mod.rs`. It provides, as the
design's §2 and §4 describe them:

- **The name rules** (§3): a canonical form and the errors for each way a
  name can fail; the display casing checked against it.
- **The certificate** (§2's fields), its wire form (`postcard`, under a
  256-byte cap checked before decoding), a domain-tagged signing payload,
  and a parse that is total: any bytes are a certificate or an error, never
  a panic, with the name rules re-checked and the lifetime sane.
- **Verification** as §5's pure function: the certificate, the joining
  UUID, the clock, the known registry keys and an optional revocation list
  in; the vouched name or a reason worded for a disconnect out. Five minutes
  of clock skew. A certificate signed by a retired key is good if it was
  issued before that key's rotation, until it expires.
- **The registry's keys** and their **pre-committed rotation**: a rotation
  record signed by the retiring key names its successor; the successor is
  accepted only if it hashes to the commitment already held (use
  `keyset::commit_to`); the record carries the next commitment.
- **The revocation list**: signed by the registry, capped in bytes and in
  entries, keyed by registry key and serial.
- **Claim and release requests** (§4): signed by the player's own
  `Identity`, with a timestamp refused beyond the skew and, on a claim, a
  proof of work — BLAKE3 over the request with a nonce, a configurable
  number of leading zero bits, about a second at the default. Whether the
  signing key belongs to the UUID is the service's question (N5); here it is
  the caller's.
- **Constants** from the design: 3–18 characters; 90-day lifetime; renewal
  under 30 days; 5-minute skew; the caps.
- **Tests**, in the module: every name rule; a certificate round trip under
  the cap; verify accepting a good one and refusing a forged signature, a
  wrong UUID, an expired one, one not yet valid, an untrusted key, and a
  revoked serial; a rotation accepted against its commitment and refused
  without it; a retired key still good for an earlier certificate; a claim
  signed and verified, stale refused, insufficient work refused; the proof
  of work solved and checked at a small bit count.
- **A fuzz target**, `fuzz/fuzz_targets/name_certificate_decode.rs`, with
  its `[[bin]]` entry: parsing the certificate, the rotation record, the
  revocation list and the requests never panics on any bytes, and whatever
  parses satisfies the rules the parser promises. Charter rule 14: in the
  same commit as the parsers.

No new dependencies: `ed25519-dalek`, `blake3`, `postcard`, `serde` and
`thiserror` are already core's. Nothing here is simulation, so the
determinism rules do not apply, but the clippy configuration still does.

### After N2

N5 (`crates/registry`, design §7) adds a workspace member and new
dependencies: an HTTP server, `lettre` for mail. Each must pass `cargo deny
check` (MIT or Apache-2.0 are safe; the policy is in `deny.toml`) and
should take `default-features = false` where the crate allows. N3 bumps
`PROTOCOL_VERSION` and follows CONTRIBUTING's protocol section. N4 and N6
follow; the engine's agent will brief each from the design when N2 and N5
have landed.

## House rules that fail quietly when broken

- Two SPDX lines at the top of every new source file, in that file's
  comment syntax: `SPDX-FileCopyrightText: Iridesium` and
  `SPDX-License-Identifier: GPL-3.0-only`.
- Rust edition 2024. `thiserror` for errors. No `unwrap()` or `expect()`
  outside tests. Every public item documented, and the documentation says
  *why*, as the existing modules do. Small modules over large ones.
- These must be clean before a commit:

  ```sh
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test -p tiamat-core identity::registry
  ```

  and, where a nightly toolchain is available, the fuzz target builds:

  ```sh
  cargo +nightly fuzz build name_certificate_decode --target x86_64-unknown-linux-gnu
  ```

- **Commits.** Conventional messages (`feat(identity): …`), one working
  increment each, **signed off** (`git commit -s`; the author is Iridesium,
  and CI rejects a commit without a matching trailer). Add a
  `Co-Authored-By:` line naming the assistant if you like; it is not required.
- **The checkout is shared.** Other agents work in this same working copy.
  Stage files by name, never `git add -A` or `git add .`; never touch
  `docs/engine-asks/*`, `game/`, or anything you did not change.
- **Branch `feat/accounts`**, off `main`. Never push to `main`: the engine's
  agent runs the full gate on the branch and merges it.

## Handing a row back

Say which files changed, paste the output of the three commands above, and
list anything the design got wrong or left unsaid, with the fix made to
`docs/accounts-design.md` in the same commit. A row is done when the engine's
agent has gated and merged it.
