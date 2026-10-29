<!-- SPDX-FileCopyrightText: Iridesium -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Accounts — a name registry at tiamatengine.com

*Draft 2, 2026-10-21: draft 1 with the designer's ten decisions folded in
(§11, the same day). The design the designer asked for, to ship with 0.5.0
if it can. It is the second of the two answers laid out on
2026-10-14 to "players keep changing names"; the first, `sticky_names`,
landed as N1 and stays. Everything here is checked against the engine as it
stands: `crates/core/src/identity` (keys, key sets, `AuthProvider`),
`crates/core/src/session` (how a join binds a name), the protocol's `Hello`,
and `docs/distribution.md` §2 (how the update host's key is pinned and
rotated). Where the text and the code disagree, the code wins and the
disagreement is fixed here. §11 records what the designer decided.*

---

## 0. One paragraph

An **account** is a name that is yours everywhere, bound to the identity you
already have. The engine's identity is an Ed25519 key set with a BLAKE3 UUID
that never changes (charter rule 13); today a display name is whatever the
client asks for, claimed per server first-come. The registry at
`registry.tiamatengine.com` makes a name global: a player proves they hold
their key, the registry issues a short-lived **name certificate** signed by
Iridesium's registry key, the client presents it when joining, and a server
checks the signature offline against a key it already has. No server ever
calls the registry; LAN and singleplayer work exactly as now; the registry
being down means a name cannot be claimed or renewed that day, never that
someone cannot play. There are no passwords, no logins, no email required:
the recovery phrase that recovers the key recovers the name with it.

## 1. What it is, and is not

**It is:** one unique name per identity, claimed once, shown on every server
that honours the registry, surviving device loss (the phrase) and key
rotation (the UUID), renewable without attention, releasable at will, and
revocable by Iridesium on a report.

**It is not:** a login (keys stay the identity), a password store, a friends
list, a server list, a ban list shared between servers, a profile with an
avatar, or a social graph. Any of those is a later service behind the same
door. It is also not a change to how mods see players: the UUID stays the
only key a mod may store things under; a name is a display string, now one
the server can vouch for.

**The honest limit stands.** Keys are sybil-cheap, so identities are free
and so are names up to the rate limits; the registry makes a name *unique*,
not a person *accountable*. Bans still key on UUID and IP.

## 2. The model: name certificates

```
NameCertificate (postcard, then signed; ≤ 256 bytes on the wire)
  version        u8          1
  name           String      3–18 bytes, in canonical form (§3)
  display        String      the same name in the holder's casing
  uuid           [u8; 32]    BLAKE3 of the root public key, as the engine's PlayerUuid
  issued         i64         Unix seconds
  expires        i64         Unix seconds; issued + LIFETIME
  serial         u64         monotonic per registry key, for revocation
  registry_key   [u8; 32]    which registry key signed it
  signature      [u8; 64]    Ed25519 over the fields above, by the registry key
```

- **Bound to the UUID, never to a key.** A player's key set rotates and
  adds devices with the UUID unchanged (`identity::keyset`), so a certificate
  follows them through every rotation and onto every device that holds a key
  of the set. The client proves it holds *a* key of that UUID at the join
  exactly as today; the certificate adds only "and this UUID is called X".
- **Short-lived, renewed silently.** `LIFETIME` is 90 days (§11); the client
  renews whenever it is online with fewer than 30 days left. A certificate
  that lapses still names the player on servers until it expires, and the
  name stays theirs at the registry for the inactivity period (§4) before
  anyone else may claim it. Short lifetimes are the revocation mechanism a
  server that never calls home can rely on.
- **Verified offline.** The engine ships the registry's current public key
  and, as the update manifest does for the release key (`distribution.md`
  §2), the BLAKE3 hash of the *next* one. A rotation is a signed record the registry publishes and the client fetches
with its renewal and carries on every challenge answer (§5); a server accepts
the new key only if it matches the hash it already had. It applies the records
per join, to a copy of the key it pins, and never moves the pin itself: that
is the operator's, who is told once when a join proves the registry has moved
past it. A server built before a rotation still verifies certificates signed
by the key it knows until they expire, which is the 90 days it takes for every
client to renew under the new key. Once a join's records retire a key, that
key is honoured only for a certificate issued before the rotation and expiring
no more than 90 days after it, so a stolen retired key cannot back-date a
year-long certificate.
- **Hostile input, as everything from a client is** (charter rule 14):
  fixed-size fields, one bounded string, parsed before any allocation, with a
  `cargo fuzz` target in the same change as the parser.

## 3. Names

- **Canonical form** is what is unique and what the certificate carries:
  3–18 characters from `a–z`, `0–9` and `_`, starting with a letter. The
  player's **display casing** (`Iridesium`, `IRIDESIUM`) is theirs to pick at
  claim time and travels in the certificate beside the canonical form; two
  players cannot hold `alice` and `Alice`. This rules out every confusable that differs only in case; the few left inside `a–z0–9_` (`l`, `1`, `I`; `0`, `O`) are folded only against the reserved list, by the owner's decision that lookalikes between players are allowed. A Unicode profile (PRECIS, with confusable
  folding) is the documented way to widen it in a later version without
  breaking uniqueness, since any wider name would fold to a canonical form
  this rule never issued. Eighteen fits the protocol's 32-byte name limit
  with room.
- **Reserved**: `iridesium`, `tiamat`, `admin`, `server`, `system`, `console`,
  `moderator`, `support` are the engine's own list; every default mod's id and
  the operator's short list are **the registry's**, seeded from
  `REGISTRY_RESERVED` into its table at startup — the engine must not carry a
  mod's name, and an engine test enforces it. Reserved names, and their lookalikes under a fold of `l`, `1`, `I` to `i` and `0` to `o`, are refused with the reason; a name held before it or its lookalike was reserved keeps renewing.
- **One name per identity.** A rename is a release and a claim; the released
  name is held for the same UUID for 30 days so a slip is undone. Renames are
  limited to three in any 30 days per identity.
- **No profanity filter at claim time.** Lists are porous and insulting in
  their misses; the remedy is a report and the operator's revocation (§4),
  with the audit log saying what was done and why.

## 4. The flows

**Claim.** The client builds `{ name, display, uuid, public_key, timestamp, work }`,
signs it with its current key, and POSTs it. The public key travels in the
request, as `Hello` carries one, so the registry can check the signature and
then ask whether that key belongs to the UUID. `work` is a small proof of
work (§7), a second or so of the claimant's CPU, which makes a thousand
names cost a thousand seconds and a single one cost nothing anyone notices.
The registry checks the signature against the key, that the key's UUID
matches (a new identity's root key *is* its UUID; an added device's key
proves itself by the key-set record the client sends alongside, exactly as
a server verifies it), the work, that the name is free and not reserved,
and the limits; it writes the binding and answers with the certificate. The
client stores it beside `identity.key`.

**The key-set record is a chain from the root.** `KeySetProof` in
`core::identity::registry`: the root public key, then one link per key event —
the root's own successor commitment, an `add-key` for a device, or a
`rotate-key` handover. An add link carries the payload the client signed when
that device was added — its key, the successor it committed to, the key that
authorised it, and that key's signature. A rotation link is the `rotate-key`
payload: the current controlling key names its pre-committed successor and
signs the handover, and the successor's own commitment is carried forward. The
last link's key is the one that signed the request. The registry verifies
every link, that each commitment is the hash of the key it hands over to, and
re-derives the UUID from the root, so a proof cannot be moved to another
identity. A root with no commitment cannot be rotated; the registry records
the first commitment it is shown for an identity and refuses any other, which
is what stops a thief holding the root rotating it to a key of their own.

**The registry remembers what it has seen rotate.** Per identity it persists
the keys a presented rotation retired (and the successor each went to) and the
first successor commitment the root declared. From then on it refuses a request
signed by a retired key, or whose proof chains through one — the root-by-hash
shortcut included; a rotation of a retired key to any other successor; and a
root declaring a second commitment. A device a retired key authorised is
refused until the successor re-authorises it. **A rotation protects nothing
until it is presented**, because the registry only learns of it from a request
that carries it: the old key still acts there until the first claim, renewal,
release, email change or recovery request presents the handover. A player who
suspects a theft should therefore do one of those promptly, not only rotate at
the game server. Claims, renewals, releases, email changes, recovery starts and
recovery cancellations all carry a proof when the signing key is not the root;
email and recovery used to be root-only and are not any more.

**Renew.** The same request with the same name, without the work; answered with a fresh certificate while the binding stands once the
current one has fewer than 30 days left; before that, with the current
certificate again (same serial and expiry, in the casing asked for), which
does not count as a renewal for the expiry rule.

**Release.** Signed like a claim; the binding is marked released with the
30-day hold.

**A new device.** The phrase restores the key, the key is the identity, the
identity is the UUID the certificate names: the client renews on first
contact and the name is there. Nothing to design.

**A recovery email, optional.** A player may bind one address to their
identity: the client sends `{ uuid, address, timestamp, work }` signed by the
key, with the key-set proof beside it when the key is not the root; `work` is
the proof-of-work nonce an identity that holds no name pays for its mail (the
claim's shape and difficulty), carried outside the signature as every
proof-of-work nonce is. The registry mails the address a link; the address is
stored only once the link is followed, and until then nothing is kept but a
hashed token that dies in a day. Changing or removing the address is the same
signed request. Nothing else ever goes to that address: no news, no notices of
joins.

**Everything lost — no phrase, no device — with an email bound.** On a new
identity the player asks to recover the name: the new key signs `{ name,
new_uuid }`, the registry mails the bound address a link to a confirmation page, and
pressing its button (a POST; opening the link alone does nothing) starts a
**fourteen-day public notice**: the name's public page
(§7, `GET /v1/name/`) says a recovery is pending, every renewal the current
key makes in that time is answered with the same notice, and the current
key can **cancel** it with one signed request, which is what stops a stolen
mailbox from taking a name whose holder is still playing. When the notice
ends uncancelled, the binding moves to the new UUID, the email binding with
it, and every certificate issued to the old UUID that could still verify is put on
the revocation list by serial. A player without a bound email who loses both phrase and
device has lost the name; the charter's honest limit for the identity
stands and the registry inherits it.

**Report and revocation.** A form on tiamatengine.com, read by the operator
(Iridesium), who can **revoke** (the serial goes on the signed revocation
list the registry publishes and clients fetch with renewals; servers that
have internet may fetch it too, but need not), **reserve**, or **rename**
through the admin subcommand (§7). The list carries the registry key that
signed it and, with each entry, the key that issued the revoked
certificate, so a rotation does not make an old serial ambiguous with a
new key's numbering. Every action is a row in the audit table with the
reason. Deliberately manual, as the charter's `rebind` is.

**Expiry.** A name whose certificate a client has renewed at least once is
released after 12 months without a renewal; a name claimed and **never
renewed** is released when its first certificate expires at 90 days. Both
with the 30-day hold. The second rule is for the squatter who claims and
walks away.

## 5. The server

The challenge answer (`ClientMessage::AuthResponse`) gains an optional
`name_certificate: Option<Vec<u8>>` (protocol 94; bounded at 256 bytes,
refused above) and, after it, `registry_rotations: Vec<Vec<u8>>`: the
registry's rotation records the client holds, oldest first, at most four of at
most 512 bytes each, refused above. Both ride with the proof of the key, which
is the moment the session can check them. **Not on `Hello`:** `Hello` is the version gate, decoded
before the versions are compared, so a field added to it would turn a
mismatched pair's greeting into a malformed-message disconnect, in both
directions, instead of the version message that tells a player to update;
`Hello`'s shape never changes, and a protocol test pins it. The session's join
(`core::session::JoinContext`) gains one input, the verified certificate's
name, and one rule:

- **A valid certificate wins its name.** The session binds the certified
  canonical name with the certificate's display casing. If an uncertified
  player on this server already wears it, *they* are renamed to a numbered
  variant and told why — a new `WhyRenamed` reason, "that name is
  registered to somebody else". `sticky_names` applies to uncertified
  players only; a certified player's name follows their certificate, so a
  rename at the registry is seen at the next join.
- **An uncertified player may not take a certified player's name**, and
  nothing more: a server knows only the certificates it has been shown, so
  an uncertified player can wear a name the registry has issued to someone
  who never joined here. That is the price of never calling home, and it is
  the right price; the moment the holder joins, the name is theirs.
- **Policy, `registry` in server.toml:** `honour` (the default; what is
  above), `require` (a public server: no valid certificate, no join; told
  why), `off` (certificates ignored; a LAN world, an embedded host).
- **The pinned strings, and a startup that refuses a mistyped one.**
  `registry_key` carries the registry's current public key (64 hex
  characters) and `registry_next_key_hash` the BLAKE3 of its pre-committed
  successor (also 64), the pair design §2 says the engine ships. Both are
  shape-checked at load, not at the first join; `require` without a key
  refuses to start, because a server in that state would turn every player
  away and look broken rather than misconfigured; and a successor hash
  without a key is refused as a trust set with nothing to rotate from.
  The successor is only checked for shape: nothing can prove it belongs to
  the current key until a rotation record arrives, which is what
  pre-commitment means. A record arrives with a player. The session applies
  the carried records to a copy of the pin (`RegistryTrust::advanced_by`):
  they must form one contiguous chain that reaches the pinned key; records
  older than the pin are passed over and trusted for nothing; from the pin
  on, each must be signed by the key it retires, name the committed
  successor, and not be stamped in the future. A chain that does not hold
  leaves the join uncertified under `honour` and refused under `require`,
  and the certificate is verified against the chain's end only. The first
  join in a process that proves a rotation past the pin logs a warning
  naming the new `registry_key` and `registry_next_key_hash`, and when
  pinning them costs nobody their name: ninety days after the last handover.
  The operator confirms both against `GET /v1/keys` before pinning: a stolen
  retiring key could sign a second record to the same committed successor
  with a different next hash, which cannot vouch for any key but could, if
  pinned, block the genuine next rotation.
- **RCON `rename`** on a certified name is refused with the reason, whether
  its holder is here or not: the registry owns that name, and a rename would
  be undone at their next join and would displace whoever had taken it
  meanwhile. The mark is kept with the binding and persisted. A certified
  join sets it; it is cleared when the binding moves (displacement, release,
  rename) or when the identity next joins uncertified. Under `off`, or with
  no key pinned, no certificate would bring the name back, so the mark does
  not refuse. `game.player_certified` still answers for this join. `rebind`
  is unchanged.
- **Verification** is a pure function in core: the certificate's bytes, the joining UUID, the clock, the known registry
  keys (the pin, advanced through the carried rotation records) and the
  revocation list (if the server has one) in; the name or a reason out. The `AuthProvider`
  trait is untouched: this is not authentication, and the trait stays the
  door for a community identity service that *is*.
- **Mods** see the name as today. One addition to the API (1.x, additive):
  `event.certified` on the join hook and `game.player_certified(uuid)`, so a
  mod may show a mark or refuse the uncertified on a server that chose to.

## 6. The client

The client keeps the registry's rotation records beside its certificate
(fetched with each renewal from `GET /v1/keys`) and sends its most recent
four on every challenge answer, so a server pinned before a rotation can
still verify the certificate (§5).

- **The identity screen** (where the recovery phrase lives) gains the
  account: the name held, its expiry, *Claim a name* / *Rename* / *Release*,
  the recovery email with *Bind* / *Change* / *Remove* and its state
  (unverified, verified), *Recover a name* for a fresh identity, and a line
  saying what is sent (§8). Claiming is a form with the name field and the
  registry's answer in words: taken, reserved, too short, too many attempts,
  done. The proof of work runs while the form is open, with a small progress
  mark, so the claim itself is instant.
- **Renewal is silent**, on launch and once a day while running, when online
  and under 30 days remain. A failure is a line in the log and a quiet mark
  on the screen; never a dialog. A pending recovery against this identity's
  name is the one thing renewal surfaces, as a notice with a *Cancel* button.
- **The registry's URL and public key are pinned** in the client beside the
  update host's, with `--registry <url>` and a key override for tests
  against a local instance, as the updater has for its manifest.
- **Offline** is nothing special: the stored certificate is sent; if it has
  expired the join is uncertified and the server treats it as such.
- **Privacy, said on the screen:** what leaves the machine is the name, the
  public key, a signature and the timestamp; the registry sees the IP address
  for rate limiting and keeps it for seven days; an email is kept only if
  the player binds one, only for recovery, and is removed on request. No
  telemetry.

## 7. The service

`crates/registry`: a headless binary like the server, GPL-3.0-only, no GUI,
no engine simulation, with subcommands `serve`, `healthcheck` (the image's `HEALTHCHECK`: `GET /healthz` on loopback) and `admin`. It speaks plain
HTTP inside its container; TLS is Dokploy's Traefik, as it is for the update
host. Storage is SQLite through `rusqlite` (already in the tree; the WAL
policy of `persist::checkpoint` applies), signing is `ed25519-dalek` (in the
tree), mail goes out over SMTP with `lettre` (MIT, rustls), and the HTTP
side is the smallest pure-Rust server in good standing (`tiny_http`, or
`hyper` alone), chosen by `cargo deny` and dependency count rather than
fashion.

| endpoint | does |
|---|---|
| `POST /v1/claim` | §4 claim or renew; answers the certificate |
| `POST /v1/release` | §4 release |
| `POST /v1/email` | bind, change or remove the recovery address; `GET /v1/email/verify?t=` is the link in the mail and shows a page whose button `POST`s the token to the same path |
| `POST /v1/recover` | start a recovery; `GET /v1/recover/confirm?t=` is the link's page and `POST /v1/recover/confirm` its button; `POST /v1/recover/cancel` is the holder's veto |
| `GET /v1/name/{canonical}` | whether a name is held, by which UUID, and whether a recovery is pending — public, so a server with internet may ask, and so a player can check before claiming |
| `GET /v1/keys` | the current registry public key, the hash of its successor, and the signed rotation records |
| `GET /v1/revocations` | the signed revocation list, small, cacheable |
| `GET /healthz` | up, and the database answers |

**Tables:** `names` (canonical, display, uuid, claimed, renewed, released,
hold_until, renewals), `emails` (uuid, address, verified_at), `tokens`
(hash, purpose, uuid, expires), `recoveries` (canonical, from_uuid, to_uuid,
confirmed_at, notice_until, state, ended_at), `reserved`, `revocations` (serial, when,
reason), `issued` (serial, registry_key, uuid, canonical, issued),
`revoked_holders` (canonical, uuid, revoked_at — an operator's revocation,
which bars that identity from the name), `audit` (who, what, when, why, ip — the client address, cleared seven days after the row),
`limits` (scope — an address, a `/24` or `/48`, an identity, or the BLAKE3 of a folded mail address — window, count; a row is deleted by the service itself seven days after its window ends).

**Limits, set for the bulk squatter** (the designer, §11.8): two claims a
day per IP address and six per `/24`; one claim and three renames per
identity in any 30 days; a proof of work on every claim — BLAKE3 over the
claim with a nonce, leading zero bits set by the service and raised while a
subnet's claim rate is high, so a burst from one place pays more for each
name; a 32 KiB body cap; a bounded pool of workers (`REGISTRY_WORKERS`, 8) that answers `503` when it and its queue are full; a 2 s deadline per request from the listener to the answer, body included, past which work that has not committed is rolled back and answered `504` (committed work is answered with its result, however late); refusal of any request whose timestamp is more than five minutes from the service's clock (replay); and at most three casing changes per identity in any 30 days outside a renewal that mints. Mail is limited to three messages a day per identity, three an hour per address (lower-cased, `+tag` removed, keyed by its hash), ten an hour per `/24`, and `REGISTRY_MAIL_PER_HOUR` (60) an hour in all; an email request from an identity that holds no name carries a proof of work of the claim's shape and difficulty. Beside the limits, the operator's view: `admin claims --since 48h`
groups young claims by subnet, and `admin revoke --subnet 203.0.113.0/24
--since 48h --reason "…"` undoes a burst in one line, every name of it to
the audit table.

**Keys.** The registry key and its successor are generated **offline** with
`registry admin key new`, on the designer's machine beside the release key
(`C:\tiamat-keys` is the precedent). The host receives the current key as a
Dokploy secret and the successor's BLAKE3 hash as a plain variable; the
successor's private half never leaves the designer's machine until a
rotation, when it is uploaded as the new current key, a new successor is
made, and the service publishes the rotation record signed by the old key.

**Deployment on Dokploy**, step by step in `docs/registry-ops.md`:

1. The engine's release workflow builds a small two-stage image
   (`crates/registry/Dockerfile`: the Rust build, then a minimal runtime
   with the one binary) and pushes it to the GitHub container registry as
   `ghcr.io/iridesiumresearch/tiamat-registry:<version>`, public. Dokploy
   never compiles Rust.
2. In Dokploy, a new Application with provider *Docker image*, that image;
   a volume mounted at `/data` for the database, the daily backups and the
   logs; the domain `registry.tiamatengine.com` with HTTPS, which Traefik
   certifies; the image's own `HEALTHCHECK` (`registry healthcheck`), so none is configured; Traefik's buffering middleware (`maxRequestBodyBytes=32768`) on the router; environment: `REGISTRY_DATA`,
   `REGISTRY_KEY` (secret), `REGISTRY_NEXT_KEY_HASH`, `REGISTRY_SMTP_URL`
   (secret, `smtps://user:pass@host`), `REGISTRY_MAIL_FROM`, `REGISTRY_BASE_URL`.
3. DNS: an `A` record for `registry` to the Dokploy host.
4. Updates are a new image tag in the application's settings and *Redeploy*;
   the database migrates itself forward on start, never back.

**Backups.** The service writes `VACUUM INTO /data/backups/registry-<date>.sqlite`
once a day and keeps fourteen. Dokploy's volume backup to S3-compatible
storage covers the volume where the installed version offers it; where it
does not, a second tiny service in the same project runs `rclone` on a
schedule against the same volume. Either way a restore is: new volume, copy
the newest file to `/data/registry.sqlite`, start.

**Mail.** Any SMTP provider the designer prefers, transactional, with the
`From` address on tiamatengine.com and that domain's SPF and DKIM records
set for it; the service sends plain text with one link and nothing else.

**Operations, the rest:** the engine's nightly workflow asks `/healthz` and
`/v1/keys` and fails its job if either is wrong; `admin stats` prints names
held, claims and recoveries by day, and the limits tripped.

**Legal, the designer's:** a terms page and a privacy page at
tiamatengine.com (§8 in plain words, plus retention and the report address);
deletion on request is a release and the removal of the email.

## 8. What is sent, kept, and for how long

| datum | sent to | kept | why |
|---|---|---|---|
| canonical and display name, UUID | the registry, then every server joined | while bound; a released or lapsed name's row kept 30 days after its 30-day hold ends, then deleted | the binding |
| public key, signature, timestamp, the proof of work | the registry | not kept past verification | proof of possession |
| IP address, and its `/24` | the registry | seven days (the daily backups keep up to fourteen more) | rate limiting |
| recovery email address | the registry | while bound; removed on request, or with the identity's last name, or 30 days after it was verified for an identity that never held one | recovery only |
| verification and recovery tokens | the registry | hashed, with the pending address; deleted at expiry, a day | the links in the mail |
| recovery records | the registry | ninety days after they end | the operator's record of a disputed name |
| audit rows | the registry | indefinitely, without the client address after seven days | the operator's record of claims, renames, revocations and mail requests |
| certificate serials issued (serial, key, UUID, name) | the registry | while the certificate can verify (90 days and the skew) | revocation |
| rate-limit counters (address, `/24`, identity, hashed mail address) | the registry | a week after their window | rate limiting |

Nothing else. The registry never learns which servers a player joins, and
never writes to a bound address except to verify it or to confirm a recovery
the player asked for.

## 9. Charter check

- **Rule 13.** Identity is still the key set and the UUID is still canonical
  and unchanged by anything here; names are still display strings, now
  vouched for. The sybil limit is stated, not hidden (§1). `AuthProvider`
  stays pluggable and untouched.
- **Rule 2.** The server is the game and verifies alone; no network in the
  join path; the embedded singleplayer host ignores certificates by default
  policy (`off`), since it has no one to impersonate.
- **Rule 14.** The certificate is parsed as hostile, bounded, fuzzed.
- **Rule 4.** Nothing here touches simulation.
- **Rule 17.** The service crate carries the same licence and headers; its
  dependencies pass `cargo deny`.

## 10. Building it, and 0.5.0

**Engine (the agent):** the certificate type, its codec, verification and
fuzz target in `core::identity::registry`; the `AuthResponse` field and
protocol 94;
the session rule and the new `WhyRenamed`; the `registry` server policy and
the RCON refusal; the client's store, renewal, pinned key, the proof of
work, and the screen with the email and recovery forms; the API addition
for mods; bot tests against a registry instance the test starts in-process
(the service crate is a library with a binary, so a test calls
`registry::serve` on a loopback port with a mail sink in place of SMTP):
claim and join as the certified name, an impostor renamed, a forged
certificate refused, an expired one uncertified, a rotation accepted only
against its commitment, `require` refusing the uncertified, a recovery
confirmed by mail and cancelled by the holder, and one that completes and
revokes the old serials. About eight working days.

**Service (the agent):** `crates/registry` with `serve` and `admin`, the
tables and their forward migrations, the limits and the proof of work, the
mail, the Dockerfile and the image job in the release workflow, the ops
page. Four to five days, overlapping the above.

**Operations and legal (the designer):** the DNS record, the Dokploy
application from the published image with its volume, domain and
variables, the registry key and its successor made offline and the hash
recorded, an SMTP provider and the domain's SPF and DKIM for it, the terms
and privacy pages, the report address. Days of elapsed time; an afternoon
or two of work, in the order `docs/registry-ops.md` gives.

**0.5.0.** The engine side and the service can be built and gated inside
the 0.5.0 window as it stands, since the signing identities (S1, S2) have
days of lead time of their own. The release ships in one of two states, and
the designer has accepted either: the registry live, and *Claim a name*
works on day one; or the registry not yet deployed, the client's pinned URL
unset, and the screen saying the registry is not open yet — every server
already honours certificates, so the day the service goes live nothing
ships again. What must not happen is 0.5.0 waiting on the host.

**Milestone rows** (`docs/milestone-0.5.0.md`): N2 the certificate and its
verification in core, with the fuzz target; N3 protocol 94, the session
rule, the server policy, the RCON refusal; N4 the client's store, renewal,
proof of work and screen, with the email and recovery forms; N5
`crates/registry`, the admin subcommand, mail, the image and the ops page;
N6 the bot tests against an in-process registry; N7 (designer) the host,
the keys, the mail provider, the pages; exit criterion **E10:** a player
claims a name on one machine, restores the phrase on another, joins a
`require` server there under that name, and an impostor on the same server
is renamed and told why; and on the test instance a recovery by mail
completes against a silent holder and is cancelled by a present one.

## 11. Decided, 2026-10-21

1. **Names:** `a–z 0–9 _`, **3–18**, letter first, free display casing.
2. **One name per identity;** rename is release and claim, 30-day hold.
3. **Lifetimes:** 90-day certificates renewed under 30; 12 months without
   renewal frees a renewed name, 90 days frees one never renewed.
4. **Email recovery is built for 0.5.0** (§4, §6, §7, §8).
5. **Default server policy `honour`;** public servers opt into `require`.
6. **Hosting on Dokploy**, beside the update host: a published image, a
   volume, Traefik's TLS (§7).
7. **Domain** `registry.tiamatengine.com`.
8. **No fee**, and **stricter on bulk squatters**: the per-IP and per-subnet
   limits, the proof of work that rises with a subnet's rate, the 90-day
   expiry of a name never renewed, and the operator's bulk revocation (§7).
9. **Reserved names:** §3's list; nothing further known.
10. **Ship state:** either of §10's two.
