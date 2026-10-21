<!-- SPDX-FileCopyrightText: Iridesium -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Accounts — a name registry at tiamatengine.com

*Draft 1, 2026-10-21. The design the designer asked for on 2026-10-21, to
ship with 0.5.0 if it can. It is the second of the two answers laid out on
2026-10-14 to "players keep changing names"; the first, `sticky_names`,
landed as N1 and stays. Everything here is checked against the engine as it
stands: `crates/core/src/identity` (keys, key sets, `AuthProvider`),
`crates/core/src/session` (how a join binds a name), the protocol's `Hello`,
and `docs/distribution.md` §2 (how the update host's key is pinned and
rotated). Where the text and the code disagree, the code wins and the
disagreement is fixed here. §11 is what the designer decides.*

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
  name           String      ≤ 32 bytes, in canonical form (§3)
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
  §2), the BLAKE3 hash of the *next* one. A rotation is a signed record the
  registry publishes and the client fetches with its renewal; a server
  accepts the new key only if it matches the hash it already had. A server
  built before a rotation still verifies certificates signed by the key it
  knows until they expire, which is the 90 days it takes for every client to
  renew under the new key.
- **Hostile input, as everything from a client is** (charter rule 14):
  fixed-size fields, one bounded string, parsed before any allocation, with a
  `cargo fuzz` target in the same change as the parser.

## 3. Names

- **Canonical form** is what is unique and what the certificate carries:
  3–16 characters from `a–z`, `0–9` and `_`, starting with a letter. The
  player's **display casing** (`Iridesium`, `IRIDESIUM`) is theirs to pick at
  claim time and is carried beside the canonical form; two players cannot
  hold `alice` and `Alice`. This is the smallest rule that rules out every
  confusable; a Unicode profile (PRECIS, with confusable folding) is the
  documented way to widen it in a later version without breaking uniqueness,
  since any wider name would fold to a canonical form this rule never issued.
- **Reserved**: `iridesium`, `tiamat`, `admin`, `server`, `system`, `console`,
  every default mod's id, and a short list the operator keeps in the
  registry's own table. Reserved names are refused with the reason.
- **One name per identity** (§11). A rename is a release and a claim; the
  released name is held for the same UUID for 30 days so a slip is undone.
- **No profanity filter at claim time.** Lists are porous and insulting in
  their misses; the remedy is a report and the operator's revocation (§4),
  with the audit log saying what was done and why.

## 4. The flows

**Claim.** The client builds `{ name, display, uuid, timestamp }`, signs it
with its current key, and POSTs it. The registry checks the signature against
the key, that the key's UUID matches (a new identity's root key *is* its
UUID; an added device's key proves itself by the key-set record the client
sends alongside, exactly as a server verifies it), that the name is free and
not reserved, and the rate limits; it writes the binding and answers with the
certificate. The client stores it beside `identity.key`.

**Renew.** The same request with the same name; answered with a fresh
certificate while the binding stands.

**Release.** Signed like a claim; the binding is marked released with the
30-day hold.

**A new device.** The phrase restores the key, the key is the identity, the
identity is the UUID the certificate names: the client renews on first
contact and the name is there. Nothing to design.

**Everything lost — no phrase, no device.** The name is lost with the key.
This is the charter's own honest limit for the identity and the registry
inherits it. An **optional email** bound at claim time would allow a
re-binding to a new UUID after verification and a public two-week notice;
it is the one feature that brings personal data, mail delivery and a
privacy policy with it, so it is designed here and **not built for 0.5.0**
(§11).

**Report and revocation.** A form on tiamatengine.com, read by the operator
(Iridesium), who can **revoke** (the serial goes on the signed revocation
list the registry publishes and clients fetch with renewals; servers that
have internet may fetch it too, but need not), **reserve**, or **rename**
through a local admin tool. Every action is a row in the audit table with
the reason. Deliberately manual, as the charter's `rebind` is.

**Expiry.** A name not renewed for 12 months (§11) is released
automatically, with the 30-day hold.

## 5. The server

`Hello` gains an optional `name_certificate: Option<Vec<u8>>` (protocol 94;
bounded at 256 bytes, refused above). The session's join
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
- **RCON `rename`** on a certified player is refused with the reason; the
  registry owns that name. `rebind` is unchanged.
- **Verification** is a pure function in core: the certificate's bytes, the
  joining UUID, the clock, the known registry keys and the revocation list
  (if the server has one) in; the name or a reason out. The `AuthProvider`
  trait is untouched: this is not authentication, and the trait stays the
  door for a community identity service that *is*.
- **Mods** see the name as today. One addition to the API (1.x, additive):
  `event.certified` on the join hook and `game.player_certified(uuid)`, so a
  mod may show a mark or refuse the uncertified on a server that chose to.

## 6. The client

- **The identity screen** (where the recovery phrase lives) gains the
  account: the name held, its expiry, *Claim a name* / *Rename* / *Release*,
  and a line saying what is sent (§8). Claiming is a form with the name
  field and the registry's answer in words: taken, reserved, too short, too
  many attempts, done.
- **Renewal is silent**, on launch and once a day while running, when online
  and under 30 days remain. A failure is a line in the log and a quiet mark
  on the screen; never a dialog.
- **The registry's URL and public key are pinned** in the client beside the
  update host's, with `--registry <url>` and a key override for tests
  against a local instance, as the updater has for its manifest.
- **Offline** is nothing special: the stored certificate is sent; if it has
  expired the join is uncertified and the server treats it as such.
- **Privacy, said on the screen:** what leaves the machine is the name, the
  public key, a signature and the timestamp; the registry sees the IP address
  for rate limiting and keeps it for seven days. No email, no telemetry.

## 7. The service

`crates/registry`: a headless binary like the server, GPL-3.0-only, no GUI,
no engine simulation. It speaks plain HTTP on localhost behind Caddy, which
terminates TLS with an automatic certificate; the binary therefore carries
no TLS and no certificate handling. Storage is SQLite through `rusqlite`
(already in the tree; the WAL policy of `persist::checkpoint` applies),
signing is `ed25519-dalek` (in the tree), and the HTTP side is the smallest
pure-Rust server in good standing (`tiny_http`, or `hyper` alone), chosen
by `cargo deny` and dependency count rather than fashion.

| endpoint | does |
|---|---|
| `POST /v1/claim` | §4 claim or renew; answers the certificate |
| `POST /v1/release` | §4 release |
| `GET /v1/name/{canonical}` | whether a name is held, and by which UUID — public, so a server with internet may ask, and so a player can check before claiming |
| `GET /v1/keys` | the current registry public key, the hash of its successor, and the signed rotation records |
| `GET /v1/revocations` | the signed revocation list, small, cacheable |
| `GET /healthz` | up, and the database answers |

**Tables:** `names` (canonical, display, uuid, claimed, renewed, released,
hold_until), `reserved`, `revocations` (serial, when, reason), `audit`
(who, what, when, why), `limits` (ip or uuid, window, count; rows older than
seven days are deleted by the service itself). The registry key lives in a
file on the host readable by the service alone; the successor key is made
offline and only its hash goes on the host, as the release key's does
(`C:\tiamat-keys` is the precedent).

**Limits:** a token bucket per IP (10 claims an hour) and per UUID (3 a day),
a 32 KiB body cap, a 2 s request timeout, and refusal of any request whose
timestamp is more than five minutes from the service's clock (replay).

**Admin:** `registry-admin` on the host, over the same database: `revoke
<serial|name> <reason>`, `reserve <name>`, `release <name>`, `rename <from>
<to> <reason>`, `rotate-key`, `stats`. Local only; no admin HTTP.

**Operations:** one small VPS (any provider; a few euros a month), Caddy, a
systemd unit, a nightly `VACUUM INTO` backup copied off the host, and a
check from the engine's nightly workflow that `/healthz` answers. The whole
of it is one page in `docs/registry-ops.md`, written with the service.

**Legal, the designer's:** a terms page and a privacy page at
tiamatengine.com (what §6 says, plus retention and the report address);
deletion on request is a release.

## 8. What is sent, kept, and for how long

| datum | sent to | kept | why |
|---|---|---|---|
| canonical and display name, UUID | the registry, then every server joined | while bound; released rows kept 30 days then deleted | the binding |
| public key, signature, timestamp | the registry | not kept past verification | proof of possession |
| IP address | the registry | seven days | rate limiting |
| audit rows | the registry | indefinitely | the operator's record of revocations and renames |

Nothing else. The registry never learns which servers a player joins.

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
fuzz target in `core::identity::registry`; the `Hello` field and protocol 94;
the session rule and the new `WhyRenamed`; the `registry` server policy and
the RCON refusal; the client's store, renewal, pinned key and screen; the
API addition for mods; bot tests against a registry instance the test starts
in-process (the service crate is a library with a binary, so a test calls
`registry::serve` on a loopback port): claim and join as the certified name,
an impostor renamed, a forged certificate refused, an expired one
uncertified, a rotation accepted only against its commitment, `require`
refusing the uncertified. About a week.

**Service (the agent):** `crates/registry` and `registry-admin`, the tables,
the limits, the ops page. Three to four days, overlapping the above.

**Operations and legal (the designer):** the DNS record for
`registry.tiamatengine.com`, the VPS and Caddy, the registry key made and its
successor's hash recorded, the terms and privacy pages, the report address.
Days of elapsed time; an afternoon of work.

**0.5.0, honestly.** The engine side and the service can be built and
gated inside the 0.5.0 window as it stands, since the signing identities
(S1, S2) have days of lead time of their own. The release can then ship in
one of two states, and either is sound: the registry live, and *Claim a
name* works on day one; or the registry not yet deployed, the client's
pinned URL unset, and the screen saying the registry is not open yet — every
server already honours certificates, so the day the service goes live
nothing ships again. What must not happen is 0.5.0 waiting on a VPS.

**Proposed milestone rows** (added to `docs/milestone-0.5.0.md` once §11 is
decided): N2 the certificate and its verification in core, with the fuzz
target; N3 protocol 94, the session rule, the server policy; N4 the client's
store, renewal and screen; N5 `crates/registry` with the admin tool and the
ops page; N6 the bot tests against an in-process registry; N7 (designer) the
host, the key, the pages; exit criterion **E10:** a player claims a name on
one machine, restores the phrase on another, joins a `require` server there
under that name, and an impostor on the same server is renamed and told why.

## 11. Decisions for the designer

1. **Names.** The canonical rule in §3 (`a–z 0–9 _`, 3–16, letter first)
   with free display casing — yes, or wider from the start?
2. **One name per identity**, rename = release and claim with a 30-day hold.
3. **Lifetimes.** 90-day certificates renewed under 30; 12 months without
   renewal frees the name.
4. **Email recovery:** designed, not built for 0.5.0. Confirm.
5. **Default server policy** `honour`; dedicated public servers opt into
   `require`. Confirm, or make `require` the dedicated default.
6. **Hosting:** a VPS with Caddy and SQLite as in §7, run by Iridesium.
   Confirm, or name a provider you already use.
7. **The domain:** `registry.tiamatengine.com`.
8. **Fee:** none; squatting is answered by expiry and the operator.
9. **Reserved names** beyond §3's list.
10. **Ship state** for 0.5.0: both states in §10 are acceptable; confirm.
