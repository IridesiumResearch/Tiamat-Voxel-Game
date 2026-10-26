<!-- SPDX-FileCopyrightText: Iridesium -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Accounts: the review of `feat/accounts`, 2026-10-26

The branch handed back N2, N3 and N5 of `docs/milestone-0.5.0.md` (seventeen
commits, 2026-10-23 to 2026-10-26). This is what the review found, what was
fixed before the merge, what was left open on purpose, and the decisions the
owner has to make. The implementer picks up from "Open, for the implementer";
the owner from "Decisions".

The review was three independent reads — the certificate core
(`core::identity::registry` and the fuzz target), the service
(`crates/registry`), and the protocol, session, server, client and bot — each
against `docs/accounts-design.md` and the charter, followed by the full local
gate. Line numbers below are from the branch as reviewed and will have moved.

## Verified sound

- The certificate carries no trust anchor of its own: `registry_key` only
  selects among the pinned current key and its retired predecessors, and the
  signature is checked with the pinned key. Domain separation holds across all
  eleven signed payloads. Clock arithmetic saturates; the decoders check every
  cap before postcard runs and refuse trailing bytes.
- The session verifies the certificate against the UUID the challenge proved,
  so a copied certificate certifies nobody (now also a session test). `off`
  ignores a certificate even with a key pinned; `require` refuses without one;
  `require` with no key, or a successor hash with no key, refuses at load.
- Claim and renew at the service require the UUID's root key or a key-set
  proof whose root hashes to the UUID. Tokens are random, stored hashed,
  purpose-bound and expiring; claims are one transaction under a unique
  index; migrations are version-gated; secrets stay out of logs and responses;
  admin is CLI-only.

## Fixed before the merge

### Engine side (the engine agent)

- **`Hello` had gained a field.** `Hello` is the version gate — decoded whole
  before the versions are compared — so appending `name_certificate` to it
  made a 0.4.x client and a 0.5.0 server show each other "malformed message"
  instead of the version-mismatch text, in both directions. The field now
  rides on the challenge answer (`ClientMessage::AuthResponse`), which only
  travels between peers that already agreed on a version; `Hello`'s encoding
  is pinned byte for byte by a protocol test. Design §5 says so.
- **A certified join could leave its victim nameless.** `bind_certified`
  released the holder's name before it knew a variant was free; the variant is
  now chosen first, so a refusal changes nothing.
- **A holder displaced while still fetching content kept the old name** in the
  roster and the mod API (the rename queue only finds players with a slot).
  The session now re-reads its binding from the registry on `JoinWorld` and
  tells the player why (`WhyRenamed::Displaced`).
- **A displaced holder who was certified too stayed marked certified** (a stale
  certificate after a rename at the registry): `rename_to` and the server's
  displacement path both clear the mark now.
- The accept loop's wait for the world logged nothing on failure and gave up
  after thirty seconds on a slow world open; it now waits and says why it
  stopped. The client's `NameRequired` text named an account screen that does
  not exist yet.
- `zstd` 0.13 → 0.14 (four RUSTSEC advisories against `zstd-sys`, on main too).

### Crypto core and service (the fix agent, reviewed by the engine agent)

- **Anyone could release anyone's name, or cancel anyone's recovery (A1, A2;
  B1, B2).** `ReleaseRequest::verify` and `CancelRecoveryRequest::verify`
  checked that the carried key signed the request and never that the key was
  the UUID's; the UUID is on the public name page. One private
  `verify_holder` now stands behind every player request: signature, then
  root-by-hash or a key-set proof, else `RootKeyRequired`. Release and cancel
  gained the optional `key_set`, so a second device can do both; email and
  recovery still want the root (§4). Tests: a stranger refused on all four,
  a second device accepted with its proof, at the core and over HTTP.
- **A retired registry key could vouch for a year (A5).** `signer_for` now
  also requires the record to end no later than ninety days after the key
  retired. **The pinned key could be a weak key or non-hex (A10):**
  `public_key_from_bytes` and a hex-digit check. **A claim with a two-link
  proof did not fit its cap (A6, B12):** `MAX_PROVEN_REQUEST_BYTES`, with a
  compile-time check against the HTTP body cap. **The fuzz target (A8)** now
  decodes and verifies all nine request types, the certificate through
  `verify_certificate`, rotation records, revocation lists and key-set
  proofs; a two-minute run found nothing.
- **The service did not compile on Linux or macOS**: `tiny_http::ListenAddr`
  has a `Unix` variant there. Fixed, and the crate's clippy is clean here.
- **The mailed link confirmed by GET (B3):** the link now opens a page whose
  button POSTs the token; both paths accept only a 64-hex token, pages carry
  a CSP and `nosniff`. **The name page never said a recovery was pending
  (B4):** it does, with `recovery_notice_until`. **Mail was sent under the
  database lock (B6):** sent after commit, outside it, with a ten-second
  SMTP timeout. **Unlimited renewals could evict every real revocation (B7):**
  a renewal with more than thirty days left re-issues the current
  certificate (same serial, no renewal credit); recoveries and operator
  revocations revoke only certificates that can still verify; the published
  list carries only those, and the rest are pruned. **The limit sweep wiped
  live thirty-day windows (B8):** it sweeps by window end, hourly from
  `advance()`. **A bulk revoke handed the names back (B10):** a
  `revoked_holders` table bars that identity from that name, permanently —
  the owner may prefer "waits the hold like a stranger". **A refused
  recovery link stayed usable (B14)** and **a cancel after the notice ended
  still won (B15):** fixed. Write paths begin `IMMEDIATE` (B19); a poisoned
  connection is recovered rather than fatal (B20); `parse_window` cannot
  panic (B21); the key file is created once, 0600, never overwritten (B22);
  the image runs as UID 10001 with a test on the Dockerfile (B16); the
  `net:` prefix is accepted by `admin revoke` and no match is an error
  (B18). Schema is at version 3.

## Decisions (the owner)

1. **Rotation at the registry (A3).** The service accepts the original root
   key by hash for ever and only `add-key` links in a key-set proof. A root key
   its owner rotated away from because it was stolen keeps full authority at
   the registry, and the pre-committed successor cannot prove itself at all.
   The session already refuses exactly this (`session/mod.rs`, "re-registering
   would hand the identity straight back to the key its owner deliberately
   retired"). Proposed: a rotation link in `KeySetProof` checked with
   `rotate_key_payload` and the previous link's `next_key_hash`, and a per-UUID
   record at the service of keys a presented rotation retired, refused ever
   after — including the root-by-hash shortcut. Design §4's sentence "a stolen
   device key alone still cannot claim" is misleading until then, because the
   proof travels with the device.
2. **Registry key rotation reaching servers (A4).** `registry_next_key_hash`
   is shape-checked and never consulted: nothing delivers a `RotationRecord`
   to a server, so the registry's first rotation makes every certificate
   issued under the new key `UntrustedRegistryKey` on every server until each
   operator edits `server.toml` — under `require`, every renewed player is
   turned away. Two shapes: accept in `signer_for` a key whose `commit_to`
   equals the pinned successor hash (the first signature by the successor is
   the rotation, as in pre-rotation), or carry a bounded list of rotation
   records with the certificate and apply them before verifying. The first is
   smaller; the second also delivers the *next* commitment.
3. **Confusables (A7).** Free casing re-opens them: canonical `lridesium`
   displayed as `lridesium` reads as `Iridesium` in sans fonts; `b0b` shown as
   `B0B` reads as `BOB`. Design §3's "rules out every confusable" is not true
   as written. Proposed: uniqueness and reservation decided on a skeleton that
   folds {i, l, 1} and {o, 0}, stored as a column.
4. **RCON `rename` on an offline certified player (C8).** The refusal reads the
   online set, so an operator can rename an offline certified player, whose
   next join then displaces whoever took the canonical name. Design §5 says
   "refused on a certified player" without saying online. Either persist the
   mark with the binding or say the refusal is for the player who is here.
5. **The service's concurrency shape (B5, B11).** One accept loop serves
   requests inline, so one client trickling a body stalls every other request
   until Traefik's read timeout, and the two-second deadline bounds the reply
   while the work carries on and may still commit (a claim answered 504 that
   then succeeds, so the retry gets `already_named`). Proposed: a bounded
   worker pool with the body read inside the worker under the deadline, 503
   when full, and handlers that roll back past the deadline. Traefik's
   buffering middleware (`maxRequestBodyBytes=32768`) belongs in the ops doc.
6. **Retention (B9).** §8 promises IPs for seven days, tokens for a day,
   released rows for thirty, recovery records for ninety after they end. Today
   IPs sit in `audit.why` indefinitely, expired tokens are never deleted, the
   mail quota keys hold raw addresses, and released names and finished
   recoveries are never pruned. A retention sweep in `advance()` — needed
   before the privacy page says what §8 says.
7. **Mail abuse (B13).** `/v1/email` mails any address for any fresh root key
   with no proof of work and no per-identity or global cap (3 per hour per
   lower-cased address, which `+tag` variants dodge; 10 per hour per /24 or
   /48), and `/v1/recover` lets anyone mail a holder's bound address three
   times an hour for ever. The content is fixed, so it is reputation burn, not
   phishing. Proposed: a name held or a proof of work on email requests, a
   global hourly cap, a per-UUID cap, `+tag` normalised in the quota key.
8. **Health check (B17).** The image has no `HEALTHCHECK` and no curl, and as
   far as the reviewer knows Dokploy's check runs a command inside the
   container, so the ops doc's `GET /healthz` step may not be settable as
   written; separately, when the accept loop breaks, `main` keeps waiting for
   Ctrl-C and nothing restarts it. Proposed: a `registry healthcheck`
   subcommand used by a `HEALTHCHECK`, and exit when the loop breaks.

### Decided, 2026-10-26 (the owner)

1. **Rotation at the registry (A3): the most secure shape.** A rotation link
   in the key-set proof, checked like `KeySet::rotate_key` checks a rotation
   (the retiring key's signature, the successor matching the commitment);
   the service remembers, per identity, every key a presented rotation
   retired and refuses anything signed by or chained through one, the
   root-by-hash shortcut included; the memory is persisted.
2. **Rotation reaching servers (A4): yes, with the security tested.** The
   challenge answer carries the registry's rotation records (bounded); the
   session applies them to a copy of its pinned trust before verifying the
   certificate, refuses a chain that does not start at the pin, keeps the
   retired key's ninety-day bound, and warns the operator once that
   `server.toml` has a newer key and hash to pin.
3. **Confusables (A7): no rule between players — "people will have their
   fun" — but no security hole.** The fold applies to the reserved list
   only, so a lookalike of `iridesium` or of a mod's id is refused.
4. **RCON on an offline certified player (C8): what is best for play.** The
   rename is refused offline as it is online, because an allowed one would
   be undone at that player's next join and would displace whoever took the
   name meanwhile; the certified mark is kept with the binding, and the
   refusal says so.
5. **The service's concurrency (B5, B11): best practice.** A bounded worker
   pool, the body read inside the worker under the deadline, 503 when the
   pool is full, handlers that roll back past the deadline, Traefik's
   buffering in the ops page.
6. **Retention (B9): secure, double-checked and stress-tested.** The sweep
   §8 promises, addresses hashed in quota keys, IPs in their own audit
   column cleared after seven days — with tests that drive the limits and
   the sweeps at volume.
7. **Mail abuse (B13): as proposed.** A held name or a proof of work on
   email requests, a global hourly cap, a per-identity cap, `+tag` folded in
   the quota key.
8. **Health check (B17): yes.** A `registry healthcheck` subcommand behind a
   `HEALTHCHECK`, and the process exits when its accept loop breaks.

## Open, for the implementer

- **Platform.** The crate had never been built on Linux or macOS: `let
  ListenAddr::IP(addr) = …` is refutable on unix (`tiny_http::ListenAddr::Unix`),
  so the gate's clippy and the crate's tests failed on arrival. Build and test
  on this toolchain (`rust-toolchain.toml`) before handing back; the gate runs
  here, not on Windows.
- **Revocation list consumers (A9).** `verify_certificate` takes an unverified
  `&RevocationList`; `RevocationList::verify` accepts any list a trusted key
  ever signed, including a backdated one from a retired key — and a list has
  no expiry and may name any key's serials, so a stolen retired key could
  revoke current certificates; nothing prefers the newer list either. No
  server consumes lists yet, so no impact today; a `VerifiedRevocations`
  newtype, a signing-time bound for retired keys as certificates now have,
  and keep-newest-by-`issued` before one does.
- **The two-second deadline and a ten-second mail (B11, after B6).** A mail
  send may now take up to ten seconds outside the lock while the handler's
  deadline is two, so the client can get a 504 for a mail that was delivered.
  Part of the concurrency decision above.
- **The session discards the verification reason** with `.ok()`, so under
  `require` the player gets `NameRequired` with no "expired" or "wrong
  identity"; design §5 says "the name or a reason out".
- **CI and docs drift (B23).** `registry-image` in `release.yml` has no
  `needs:` on the test jobs; no PR job builds the Dockerfile; `Cache-Control:
  no-store` on `/v1/revocations` and `/v1/keys` contradicts the design's
  "small, cacheable".
- **Test gaps the reviewers listed and this pass did not close:** a rate limit
  on renewals and the revocation list past 4096 entries; the sweep against a
  30-day window; token and IP retention; the deadline and a slow body at the
  HTTP level; the SMTP-failure path that discards the token; `client_ip`
  parsing of `X-Forwarded-For`; concurrent admin-CLI writes; a bot-level test
  for a certificate naming another identity (the session test covers it).
- **N4 and N6 remain**, as the milestone says. The client sends
  `name_certificate: None` and has no store, renewal, proof of work or forms;
  N3's bot suite covers the join half of N6, not the recovery flows.
