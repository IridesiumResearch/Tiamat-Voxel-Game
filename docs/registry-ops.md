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

A headless HTTP service over SQLite, in `crates/registry`, with three entry
points:

- `registry serve` — the service.
- `registry healthcheck` — the image's health probe (below).
- `registry admin …` — the operator commands, run beside the database.

It speaks plain HTTP inside its container. TLS is the reverse proxy's job,
because the proxy already has a certificate and a renewal story and this binary
has no business owning a second one.

**How a request is served.** The listener hands each request to a bounded pool
of `REGISTRY_WORKERS` workers (default 8), with as many places again to wait;
when every worker is busy and the queue is full, the listener answers `503
busy` at once. A request has **two seconds** from the listener to its answer,
body included: a body still arriving at its deadline is answered `408`, and work
still running at its deadline is rolled back and answered `504` — a `504` always
means nothing was changed, so the client's retry is safe. Work that has already
committed is never abandoned; the client gets its real answer, however late (a
slow mail provider after a stored link is the usual case). One slow client
therefore costs one worker two seconds, never the service. A slow client is
still the proxy's to cut off first: see *Traefik* below.

**When it stops.** If the listener stops accepting for any reason but a
requested shutdown, the process logs why and exits non-zero, so the container
is restarted instead of left running while answering nothing.

## Environment

| variable | what |
|---|---|
| `REGISTRY_DATA` | the directory holding `registry.sqlite` and its backups. **Required**, and it must be a mounted volume. Logs go to stdout, where Dokploy keeps them. |
| `REGISTRY_KEY` | the current registry key's 32-byte seed, 64 hex characters. **A secret.** |
| `REGISTRY_NEXT_KEY_HASH` | `BLAKE3` of the pre-committed successor, 64 hex characters. Not a secret. |
| `REGISTRY_BASE_URL` | the public base URL, e.g. `https://registry.tiamatengine.com`. Mail links are built from it. |
| `REGISTRY_BIND` | the address inside the container. Default `0.0.0.0:8080`. |
| `REGISTRY_TRUST_PROXY` | `1` behind Traefik, so rate limits see the real client address. Default off. |
| `REGISTRY_SMTP_URL` | `smtps://user:pass@host:465`. A secret. Set with the next one or not at all. |
| `REGISTRY_MAIL_FROM` | the `From:` address, on a domain with SPF and DKIM set for the provider. |
| `REGISTRY_WORK_BITS` | proof-of-work difficulty for a quiet network. Default 20, about a second. Leave it. |
| `REGISTRY_RESERVED` | comma-separated names nobody may claim — the default mods' ids, mostly. Seeded into the reserved table at startup. Their lookalikes are refused too (below). |
| `REGISTRY_WORKERS` | how many requests are served at once, 1–256. Default 8; each worker is a few milliseconds of SQLite per request, so more mostly queue on the database. |
| `REGISTRY_MAIL_PER_HOUR` | the most mail the registry sends in an hour, across both mails, 1–10000. Default 60. When it is reached every mail request is answered `429 limited` until the hour turns. |

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

Servers learn of a rotation from the players: the client sends the registry's
rotation records with its certificate, and a server pinned on the old key
verifies the chain and logs, once, the new `registry_key` and
`registry_next_key_hash` to put in its `server.toml`. Operators confirm both
against `GET /v1/keys` before pinning, since a stolen retiring key could sign
a second record to the same successor with a different next hash (it cannot
vouch for any other key, but pinning its hash would block the genuine next
rotation). Pinning the new key before the old key's certificates have lapsed
(ninety days) costs nobody their name.

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
   The image runs as UID 10001 (`registry`), not root. A fresh volume is
   created owned by it; a volume from an earlier image is owned by root and
   must be handed over once before redeploying, e.g.
   `docker run --rm -v <volume>:/data debian:bookworm-slim chown -R 10001:10001 /data`,
   or the service cannot open its database.
3. **Domain** `registry.tiamatengine.com`, HTTPS on (Traefik certifies it).
   Add the buffering middleware under *Traefik* below to the domain's router.
4. **Health check**: nothing to configure. The image carries its own
   `HEALTHCHECK` — every 30 s, a 5 s timeout, 10 s to start, unhealthy after 3
   failures — which runs `/usr/local/bin/registry healthcheck` inside the
   container: `GET /healthz` on loopback, healthy only on a `200`, which the
   service gives only when its database answers. Docker shows the state in
   `docker ps` and `docker inspect`, and Dokploy runs applications as Swarm
   services, which replace a task that turns unhealthy. If the application's
   *Swarm Settings → Health Check* field is set, it **replaces** the image's,
   so leave it empty, or set it to the same command:

   ```json
   {
     "Test": ["CMD", "/usr/local/bin/registry", "healthcheck"],
     "Interval": 30000000000,
     "Timeout": 5000000000,
     "StartPeriod": 10000000000,
     "Retries": 3
   }
   ```

   (Swarm takes the durations in nanoseconds.) Do not use `curl` or `wget`
   there: the image has neither. A pool that is full for a minute and a half
   fails the probe too (`503 busy`), which is a restart worth having.
5. **Environment**: the table above. `REGISTRY_KEY` and `REGISTRY_SMTP_URL` are
   secrets; `REGISTRY_TRUST_PROXY=1`; and `REGISTRY_RESERVED` set to the default
   mods' ids, because nothing else sets it and a fresh registry would otherwise
   let anyone claim one. The engine does not carry that list on purpose (a
   mod's name is the mod's), so take it from the manifests of the mods the
   deployment ships, from an engine checkout with `game/` filled:

   ```sh
   printf 'REGISTRY_RESERVED=%s\n' "$(grep -h '^id' game/*/mod.toml | cut -d'"' -f2 | paste -sd,)"
   ```

   (An id longer than eighteen characters could not be claimed anyway; listing
   it costs nothing and survives a change to the length rule.)
6. **Do not publish port 8080 on the host.** The container is reached through
   Traefik only. With `REGISTRY_TRUST_PROXY=1` the service believes
   `X-Forwarded-For`, so a client that could reach the port directly would
   choose its own address for every per-address limit.
7. **DNS**: an `A` record for `registry` on the Dokploy host.
8. Updates are a new image tag and *Redeploy*. The database migrates itself
   forward on start, never back: a newer image's schema is refused by an older
   one rather than guessed at.

### Traefik

Two things belong in the proxy, not the service:

- **Buffering.** Traefik's `buffering` middleware reads a request's whole body
  before it forwards anything, so the registry only ever reads a complete body
  from a local socket, and a body over the cap is refused at the edge with
  `413`. Define it, and list it in the `middlewares` of the router Dokploy
  wrote for the domain (in the application's Traefik configuration, a
  file-provider YAML; keep whatever else the router already has):

  ```yaml
  http:
    middlewares:
      registry-buffering:
        buffering:
          maxRequestBodyBytes: 32768
    routers:
      <the domain's router>:
        middlewares:
          - registry-buffering
  ```

  With labels instead of a file, the same is
  `traefik.http.middlewares.registry-buffering.buffering.maxRequestBodyBytes=32768`
  and `traefik.http.routers.<router>.middlewares=registry-buffering`. To check it
  took, `POST` a 40 KiB body to `/v1/claim`: the registry's own refusal is
  JSON (`{"error":"too_large",…}`), Traefik's is not.
  `32768` is the registry's own body cap; the largest real request is about
  2.5 KiB. Without the middleware the service still holds — a trickled body
  costs one worker two seconds and is answered `408` — but the proxy is the
  cheaper place to wait for a slow client.
- **Timeouts** for a client that opens a connection and says nothing: the
  entry point's `respondingTimeouts` (Traefik's defaults are fine).

### Mail

Any transactional SMTP provider with the `From` address on `tiamatengine.com`
and that domain's SPF and DKIM records set for it. The service sends plain text with one link. The link opens a confirmation
page and only its button (a `POST` to the same path) binds the address or
starts a recovery, so a mail gateway's link scanner changes nothing. It never writes to an address except to verify it or to confirm
a recovery the player asked for.

Every mail is counted against four allowances, and the first that is spent
answers `429 limited` with a sentence saying which:

| allowance | per |
|---|---|
| 3 a day | identity that asked (the binder, or the stranger asking to recover) |
| 3 an hour | address mailed — with any `+tag` removed and lower-cased, so `Alice+x@` and `alice@` are one address |
| 10 an hour | `/24` (IPv4) or `/48` (IPv6) that asked |
| `REGISTRY_MAIL_PER_HOUR` an hour | the whole registry |

An identity that **holds a name** may bind an address as it is. One that holds
none pays for the mail with a proof of work, the claim's construction over the
email request with its own domain tag, sent as a decimal nonce in a
`Tiamat-Work` header, at the difficulty a claim from the same network would pay
(it rises while the network is busy claiming names or asking for mail). Without
enough, the answer is the claim's `400 work` error with `required_bits`.

The per-address counter is keyed by a hash of the folded address, never the
address itself.

### Backups

The service writes `VACUUM INTO /data/backups/registry-<date>.sqlite` once a day
— at startup if the day's is missing, and otherwise on the first request after
midnight — and keeps fourteen. `VACUUM INTO` is a consistent snapshot even with
the database in WAL mode, so there is no "copy a half-written file" window.
A restore is: new volume, copy the newest file to `/data/registry.sqlite`,
start. If the provider offers volume snapshots, that is a second layer, not a
replacement.

A backup is a copy of the database as it was that day, so what the retention
sweep has since removed (below) is still in the backups for up to their
fourteen days.

### Retention

What §8 of the design promises to forget, the service forgets: once at
startup and then at most hourly, on the next request, one transaction deletes

| what | when |
|---|---|
| verification and recovery links | when they expire, a day after they were mailed |
| the client address on an audit row | seven days after the row; the row itself is kept |
| a released name's row | thirty days after its thirty-day hold ends (a lapsed name's likewise, from its expiry plus the hold) |
| a moved or cancelled recovery | ninety days after it ended |
| a recovery address whose identity no longer holds any name | with that name's row, or thirty days after it was verified for one that never held a name |
| a certificate's serial, and its revocation | when the certificate can no longer verify anywhere (ninety days and the skew) |
| a rate-limit counter | a week after its window ends |

A sweep that removed anything logs a line saying how many rows of each.

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
the last day, and a per-day breakdown of claims and recovery notices — a total
says nothing about whether the service is being farmed this morning.

```sh
registry admin claims --since 48h
```

Young claims grouped by `/24`, biggest first. This is the view that finds a
burst: forty names from one neighbourhood in an hour is not forty people. A
claim keeps its address for seven days, so older claims are counted under
`unknown`.

```sh
registry admin revoke --subnet 203.0.113.0/24 --since 48h --reason "bulk claim, ticket 12"
```

The burst remedy, in one line, for claims up to seven days old (the address
is not kept longer). Every name claimed from that network in the window is
released with the ordinary thirty-day hold, every certificate the holder was issued that can still verify goes on the
revocation list, and that identity may not claim the name back. `--subnet`
accepts the `net:` form `admin claims` prints; matching nothing is an error, and the reason is written to the
audit table. The names are not deleted: the rows stay for the operator's
record and the hold does what it would have done.

```sh
registry admin reserve <name>      # a name nobody may claim
registry admin unreserve <name>
registry admin rename --uuid <hex> --display <Casing>
```

`rename` changes the display casing only. The canonical form is what uniqueness
is decided on and never moves.

The default mods' ids are reserved by the **host**, not by the engine: the engine
must not know a mod's name, so the list travels in `REGISTRY_RESERVED` (or an
`admin reserve` per name) and lands in the same table. A deployment that runs
the default mods sets it to their ids; a deployment that runs none need not.

A reserved name's **lookalikes** are refused with it: a claim is compared with
every reserved name after folding `l`, `1` and `I` to `i` and `0` to `o`, so
`lridesium` and `ir1desium` are refused as `iridesium` is. The fold is for the
reserved list only; between players' names it changes nothing (`alice` and
`a1ice` may both be held). A name held before it, or before its lookalike was
reserved, keeps renewing.

## What this service does not do (yet)

- **Email and recovery need the identity's root key.** Claims, renewals, releases and recovery cancellations do not: a second device presents the signed chain of add-key authorisations from
  the root (`KeySetProof` in `core::identity::registry`), which the service
  verifies without keeping a key store of its own. Binding a recovery address
  or starting a recovery from an added device is the part still to do; the
  phrase, or the first device, works for both today.
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
with a sink transport instead. `REGISTRY_BIND=127.0.0.1:8080 cargo run -p
registry -- healthcheck` asks it whether it is healthy.
