<!-- SPDX-FileCopyrightText: Iridesium -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Running the name registry

`crates/registry` is the service behind a name that is yours on every server
that honours the registry. This page is the operator's: what it needs, how to
deploy it, and what to do when a name must be revoked. The design of record is
[`accounts-design.md`](accounts-design.md) §7; the client half is not here yet.

**Nothing on a game server depends on this service being up.** A server verifies
a certificate against a key it already has, offline. The registry being down
means a name cannot be claimed or renewed that day, never that someone cannot
play, and a certificate that lapses stops being honoured at its expiry.

## What it is

A headless HTTP service over SQLite, in `crates/registry`, with two entry
points:

- `registry serve` — the service.
- `registry admin …` — the operator commands, run beside the database.

It speaks plain HTTP inside its container. TLS is the reverse proxy's job,
because the proxy already has a certificate and a renewal story and this binary
has no business owning a second one.

## Environment

| variable | what |
|---|---|
| `REGISTRY_DATA` | the directory holding `registry.sqlite`, backups and logs. **Required**, and it must be a mounted volume. |
| `REGISTRY_KEY` | the current registry key's 32-byte seed, 64 hex characters. **A secret.** |
| `REGISTRY_NEXT_KEY_HASH` | `BLAKE3` of the pre-committed successor, 64 hex characters. Not a secret. |
| `REGISTRY_BASE_URL` | the public base URL, e.g. `https://registry.tiamatengine.com`. Mail links are built from it. |
| `REGISTRY_BIND` | the address inside the container. Default `0.0.0.0:8080`. |
| `REGISTRY_TRUST_PROXY` | `1` behind Traefik, so rate limits see the real client address. Default off. |
| `REGISTRY_SMTP_URL` | `smtps://user:pass@host:465`. A secret. Set with the next one or not at all. |
| `REGISTRY_MAIL_FROM` | the `From:` address, on a domain with SPF and DKIM set for the provider. |
| `REGISTRY_WORK_BITS` | proof-of-work difficulty for a quiet network. Default 20, about a second. Leave it. |

A service with no mail configured still serves certificates, claims and
releases; only the recovery address and recovery flows answer `503`.

## Keys, once, offline

Run these on the designer's machine, never on the host, and keep the seeds in
two places you would still have after a fire.

```sh
registry admin key new --current-out ~/tiamat-keys/registry.key \
                       --successor-out ~/tiamat-keys/registry-next.key
```

That prints the public key and `REGISTRY_NEXT_KEY_HASH`; put the current seed in
the host as `REGISTRY_KEY`, the hash as `REGISTRY_NEXT_KEY_HASH`, and the
successor's file somewhere offline. **The service cannot rotate a key whose
successor was not committed to in advance**, which is the point: a stolen
current key cannot move the registry to a key of the thief's choosing.

### Rotating

1. `registry admin key next --successor-out ~/tiamat-keys/registry-after.key`
   — a successor for the key that is about to take over. Note the
   `REGISTRY_NEXT_KEY_HASH` it prints.
2. With the **current** key still in the environment:
   `registry admin rotate --successor ~/tiamat-keys/registry-next.key --next <hash from step 1>`.
   The record is signed by the retiring key and stored in the database.
3. Restart with `REGISTRY_KEY` set from `registry-next.key` and
   `REGISTRY_NEXT_KEY_HASH` set to step 1's hash. The service refuses to start
   if the chain in the database does not end at the key in the environment,
   which is the check that catches a half-done rotation.

Clients fetch the record from `GET /v1/keys` on their next renewal. A server
built before the rotation keeps verifying with the old key until those
certificates expire, which is the ninety days every client has to renew.

## Deploying (Dokploy)

The release workflow pushes `ghcr.io/iridesiumresearch/tiamat-registry:<tag>`.
Dokploy never compiles Rust.

1. **Application**, provider *Docker image*, that image.
2. **Volume** mounted at `/data`, with the daily backups in `/data/backups`.
3. **Domain** `registry.tiamatengine.com`, HTTPS on (Traefik certifies it).
4. **Health check**: `GET /healthz`.
5. **Environment**: the table above. `REGISTRY_KEY` and `REGISTRY_SMTP_URL` are
   secrets; `REGISTRY_TRUST_PROXY=1`.
6. **DNS**: an `A` record for `registry` on the Dokploy host.
7. Updates are a new image tag and *Redeploy*. The database migrates itself
   forward on start, never back: a newer image's schema is refused by an older
   one rather than guessed at.

### Mail

Any transactional SMTP provider with the `From` address on `tiamatengine.com`
and that domain's SPF and DKIM records set for it. The service sends plain text
with one link. It never writes to an address except to verify it or to confirm
a recovery the player asked for. Limits: three messages an hour per address,
ten per `/24`.

### Backups

The service writes `VACUUM INTO /data/backups/registry-<date>.sqlite` once a day
— at startup if the day's is missing, and otherwise on the first request after
midnight — and keeps fourteen. `VACUUM INTO` is a consistent snapshot even with
the database in WAL mode, so there is no "copy a half-written file" window.
A restore is: new volume, copy the newest file to `/data/registry.sqlite`,
start. If the provider offers volume snapshots, that is a second layer, not a
replacement.

### The nightly check

The nightly workflow asks `GET /healthz` and `GET /v1/keys` at the URL in the
repository variable `REGISTRY_URL` and fails if either does not answer. The job
is skipped while the variable is unset, so the workflow is green before the
service exists.

## The operator's runbook

All commands read `REGISTRY_DATA` and `REGISTRY_KEY`, so run them with the
deployment's environment — on the host, or a copy of the volume.

```sh
registry admin stats
```

Names held and released, addresses, revoked serials, notices running, claims in
the last day.

```sh
registry admin claims --since 48h
```

Young claims grouped by `/24`, biggest first. This is the view that finds a
burst: forty names from one neighbourhood in an hour is not forty people.

```sh
registry admin revoke --subnet 203.0.113.0/24 --since 48h --reason "bulk claim, ticket 12"
```

The burst remedy, in one line. Every name claimed from that network in the
window is released with the ordinary thirty-day hold, every certificate the
holder was issued goes on the revocation list, and the reason is written to the
audit table. The names are not deleted: the rows stay for the operator's
record and the hold does what it would have done.

```sh
registry admin reserve <name>      # a name nobody may claim
registry admin unreserve <name>
registry admin rename --uuid <hex> --display <Casing>
```

`rename` changes the display casing only. The canonical form is what uniqueness
is decided on and never moves.

## What this service does not do (yet)

- **A claim must be signed by the identity's root key.** The registry is never
  shown a player's key set, so a key added to an identity on a second device
  cannot prove it belongs to the UUID here. Restoring the recovery phrase
  restores the root key and works; a device added *without* the phrase can play
  but cannot renew the name. Closing this needs a signed key-set proof in
  `core::identity::registry` and a field in the claim request — an ask on the
  engine, not a service change.
- **No fee, and no accountability.** Keys are free to mint, so names are free
  up to the limits and a UUID ban is trivially evaded. The registry makes a
  name unique, not a person accountable (charter rule 13's honest limit).
- **No API for the operator.** Every action above is a command run beside the
  database. That is deliberate: a judgement with an HTTP endpoint is a thing an
  attacker can try to automate.

## Running it locally

```sh
REGISTRY_DATA=./target/registry-data \
REGISTRY_KEY=$(head -c32 /dev/urandom | xxd -p -c32) \
REGISTRY_BASE_URL=http://127.0.0.1:8080 \
REGISTRY_BIND=127.0.0.1:8080 \
cargo run -p registry -- serve
```

No mail is configured in that invocation, so claims, renewals and releases work
and the recovery flows answer `503`. The bot tests (N6) do the same in-process
with a sink transport instead.
