<!-- SPDX-FileCopyrightText: Iridesium -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Hosting the update endpoint

**Authoritative** for where an installed Tiamat asks about updates and how that
place is run. [`distribution.md`](distribution.md) says what the client does
with the answer; this says where the answer lives, and how to put one there.

Decided 2026-09-24:

- The client asks **`https://updates.tiamatengine.com/<channel>/manifest.json`**,
  a static file host on a subdomain of its own. `test` is the channel now;
  `stable` is a second file on the same host when there is a public build.
- The archives are served by **GitHub Releases**. The host above serves a
  kilobyte of JSON and a 64-byte signature per channel, nothing bigger.
- **tiamatengine.com** itself carries the download page for humans (§8). It is
  not in the update path and can be rebuilt freely.

---

## 1. What the client actually does

Everything below follows from this, so it comes first.

1. Once per launch, on a worker thread, the front screen fetches the manifest
   URL compiled into the build, then the same URL with `.sig` appended. HTTPS
   only; redirects followed; `User-Agent: Tiamat/<version>`. The TLS roots are
   Mozilla's bundle, so a certificate from Let's Encrypt or any public CA is
   fine, and a self-signed one is not.
2. The signature is checked against the public key compiled into the binary
   (`release-key.pub`). **The host is therefore untrusted.** It can refuse to
   serve, and it cannot serve something else — which is why any static file
   host will do, and why nothing in this document is about securing a server.
3. If the manifest's version is newer than the installed one, the player is
   offered the download. Each of the artefact's `urls` is tried in order until
   one yields bytes whose BLAKE3 hash is the one the signed manifest names.
   The archive is staged; the launcher applies it at the next start.

Limits the host has to respect: a manifest is at most 64 KiB, the signature is
exactly 64 raw bytes (not hex, not base64), an artefact is at most 1 GiB, and an
artefact may name at most four URLs. A check gives the server fifteen seconds
in total; a download gives each phase up to the first byte fifteen seconds and
then a body budget sized from the archive (as long as a 128 KiB/s link would
need), so the host does not have to be fast, only present.

## 2. The URL, and why it is that one

The URL is **compiled into every shipped binary** and can never change: a build
pointed at a dead URL stops updating, and a player who stops updating cannot be
told to. Every choice below is about being able to keep it forever.

- **A subdomain of its own, not a path on the website.** `updates.` can be
  pointed at a different machine with one DNS record and no rebuild.
  `tiamatengine.com/updates/…` is tied to whatever serves the website — today
  Apache and PHP on one machine; tomorrow a page builder that rewrites paths.
- **One directory per channel**, `test/` and later `stable/`. A channel is a
  separate build with a separate URL (`distribution.md` §4).
- **Old manifests stay published** under `<channel>/releases/<version>/`. They
  are the record of what was signed, and they cost nothing.
- Never `http://`, never a query string, never a URL a content system might
  someday decide to "tidy". A dumb file server is the right amount of server.

## 3. Layout on the host

```
site/
  index.html                    one line: what this host is, and a link home
  test/
    manifest.json               the head of the channel — what clients fetch
    manifest.json.sig           its detached signature, 64 raw bytes
    releases/
      0.2.0/manifest.json       kept, forever
      0.2.0/manifest.json.sig
  stable/                       the same shape, when there is a public build
```

Two rules about the two head files. **They are the exact bytes `relman` wrote.**
The signature is over the file as served, so anything that reformats the JSON —
a pretty-printer, an editor that adds a newline, a CDN's "optimisation" —
breaks it. And **they are served with `Cache-Control: no-cache`**, so a proxy
between the host and a player revalidates rather than handing out last week's
manifest. The `releases/` copies never change and may be cached forever.

## 4. The recommended host: a static container on Dokploy, deployed from git

Why this shape: publishing a release is `git push`; the history of every
manifest is the git history; there is no server shell in the loop; Dokploy
issues and renews the certificate. Why Dokploy rather than the Apache box that
serves the website: Dokploy's proxy owns ports 80 and 443 on its machine, so it
**cannot share a machine with Apache**. It needs a small VPS of its own — a
manifest is under a kilobyte and the archives live on GitHub, so the smallest
offering anywhere is enough.

If there is no Dokploy machine yet, §5 has a host that needs no machine at all,
and moving between the two later is one DNS change (§2 is what makes that so).

### 4.1 The repository

A separate repository, `tiamat-updates` — not this one, because a manifest
publish must not trigger the engine's CI and the engine's commits must not
redeploy the host. Three things in it:

`Dockerfile`:

```dockerfile
FROM caddy:2-alpine
COPY Caddyfile /etc/caddy/Caddyfile
COPY site /srv
```

`Caddyfile`:

```
{
	# Dokploy's proxy terminates TLS in front of this container.
	auto_https off
}

:80 {
	root * /srv
	file_server
	header -Server

	# The two head files per channel: always revalidated, never stale.
	@heads path /*/manifest.json /*/manifest.json.sig
	header @heads Cache-Control "no-cache"

	# A signed release never changes, so its copy may be cached forever.
	@kept path /*/releases/*
	header @kept Cache-Control "public, max-age=31536000, immutable"
}
```

`site/`, laid out as in §3. Before the first release it holds only
`index.html`; the client answers a 404 with "could not reach", which is the
right thing for a channel with nothing on it yet.

### 4.2 DNS

Wherever tiamatengine.com's records are managed (the registrar, or Cloudflare
if the name servers were moved there), add:

| Type | Name | Value |
|---|---|---|
| `A` | `updates` | the Dokploy machine's public IPv4 address |
| `AAAA` | `updates` | its IPv6 address, if it has one |

If the DNS is on Cloudflare, set this record to **DNS only** (the grey cloud,
not the orange one): the certificate is issued by an HTTP challenge that has to
reach Dokploy's proxy directly, and there is nothing here worth a CDN. Check
from any machine:

```sh
dig +short updates.tiamatengine.com     # or: nslookup updates.tiamatengine.com
```

Do this **before** adding the domain in Dokploy; Let's Encrypt looks the name
up when the certificate is requested.

### 4.3 Dokploy

Assuming a fresh Ubuntu VPS with ports 80 and 443 open to the world (and
Dokploy's own port, 3000, open only to you):

1. Install Dokploy with the one-liner from its documentation
   (`curl -sSL https://dokploy.com/install.sh | sh` at the time of writing),
   open `http://<ip>:3000`, create the admin account.
2. Under **Git** (settings), connect GitHub so Dokploy can read the repository
   and receive pushes. The `Git` provider with a plain repository URL also
   works; it needs a deploy key for a private repository and a webhook for
   auto-deploy, both of which the application's pages show how to add.
3. **Projects → Create Project** (`tiamat`), then **Create Service →
   Application** (`updates`).
4. On the application's **General** tab: Provider **GitHub**, repository
   `tiamat-updates`, branch `main`; Build Type **Dockerfile**, Dockerfile path
   `Dockerfile`. Turn **Auto Deploy** on, so a push to `main` redeploys.
5. **Domains → Add Domain**: host `updates.tiamatengine.com`, path `/`,
   container port `80`, HTTPS **on**, certificate **Let's Encrypt**.
6. **Deploy.** The build log ends with the container running; the certificate
   follows within a minute.

### 4.4 Check it

```sh
curl -sI https://updates.tiamatengine.com/
curl -sI https://updates.tiamatengine.com/test/manifest.json
```

The first must be `200` with a valid certificate — `curl` refusing the
certificate is the thing to fix, since the client will refuse it too. The
second is `404` until the first release and `200` after, with
`cache-control: no-cache` in the headers.

## 5. Two other hosts that work

**GitHub Pages: no machine at all.** Put `site/` in a repository (with an empty
`.nojekyll` file beside `index.html`), turn Pages on for `main`, set the custom
domain `updates.tiamatengine.com` and tick **Enforce HTTPS**; the DNS record is
a `CNAME` from `updates` to `<organisation>.github.io` instead of an `A`.
Publishing is the same `git push`. Pages' CDN caches for up to ten minutes and
ignores the headers in §3, which for an update check is fine. The one honest
objection is that if GitHub is unreachable for a player, the archives are
unreachable too, so a separate host gains nothing that day — and a solo
maintainer may reasonably decide that is not the day they are designing for.

**The Apache machine that serves the website.** A `VirtualHost` for
`updates.tiamatengine.com` with `DocumentRoot /var/www/updates`, a certificate
from `certbot --apache -d updates.tiamatengine.com`, and
`Header set Cache-Control "no-cache"` for the two head files. Publishing is
`scp`. Fewest machines, but every release means logging in to the box, and the
update host now shares fate with the PHP site.

Whichever host: same layout, same URL. Moving later is a DNS change and a copy
of `releases/`.

## 6. The signing key, before anything is tagged

`release-key.pub` in this repository is the trust root compiled into every
build. **Its secret half, `release-key.hex`, has to be on the maintainer's
machine** — and nowhere else: not in this repository (`.gitignore` refuses it),
not in a CI secret, not on the update host. If the secret for the committed
public key is not in hand, generate a new pair **now, before the first tagged
release**, and commit the new public key. After the first install is out, a key
nobody holds can only be replaced by reinstalling every player by hand.

```sh
cargo run -p relman -- keygen --out ~/tiamat-keys/release-key.hex
cargo run -p relman -- show   --key ~/tiamat-keys/release-key.hex
```

`show` prints the public key (64 hex characters — that line replaces the one in
`release-key.pub`) and the **commit hash** for pre-rotation. Make the successor
at the same time:

```sh
cargo run -p relman -- keygen --out ~/tiamat-keys/next-key.hex
cargo run -p relman -- show   --key ~/tiamat-keys/next-key.hex
```

Its commit hash goes into every manifest as `--next-key-hash`. That is what
makes a rotation possible later (`distribution.md` §2): a manifest signed by
`next-key` is accepted only because an earlier manifest committed to it. To
rotate, sign with `next-key`, put its public key in `release-key.pub`, and make
a new successor. Back both key files up offline — a password manager and a
printed copy — and treat `keygen`'s warning literally: the key is the only
thing that says a release is yours.

## 7. Releasing, end to end

Once, before the first tag: in the engine repository on GitHub, **Settings →
Secrets and variables → Actions → Variables → New repository variable**,
`TIAMAT_MANIFEST_URL` = `https://updates.tiamatengine.com/test/manifest.json`.
The URL is compiled in when the tag is built; a build made before the variable
existed asks nobody, and the release workflow says so in a warning.

Then, for each release (`0.2.0` here):

1. **Tag.** Set `version = "0.2.0"` in the workspace `Cargo.toml`, commit, tag
   `v0.2.0`, push the tag. The `release` workflow builds the four targets and
   attaches the archives to a **draft** GitHub Release. Nothing is signed yet.
2. **Fetch the archives** to the machine that holds the key:

   ```sh
   gh release download v0.2.0 --dir dist
   ```

   (or from the draft release page in a browser, into `dist/`).
3. **Write the manifest.** `--protocol` is `PROTOCOL_VERSION` in
   `crates/core/src/proto/mod.rs` at the tagged commit.

   ```sh
   cargo run -p relman -- manifest \
     --dist dist --channel test \
     --commit "$(git rev-parse 'v0.2.0^{commit}')" \
     --protocol 75 \
     --key ~/tiamat-keys/release-key.hex \
     --next-key-hash <the commit hash `relman show` printed for next-key.hex> \
     --url "https://github.com/IridesiumResearch/Tiamat-Voxel-Game/releases/download/v0.2.0/{name}" \
     --notes "https://github.com/IridesiumResearch/Tiamat-Voxel-Game/releases/tag/v0.2.0" \
     --out dist/manifest.json
   ```

   `{name}` is replaced by each archive's file name. A second `--url` adds a
   mirror, tried after the first.
4. **Sign and check**, the way a client will:

   ```sh
   cargo run -p relman -- sign --manifest dist/manifest.json --key ~/tiamat-keys/release-key.hex
   cargo run -p relman -- verify --manifest dist/manifest.json --dist dist --installed 0.1.0 \
     --key "$(grep -v '^#' release-key.pub | tr -d '[:space:]')"
   ```

   `verify` re-hashes the archives in `dist/` against the manifest and checks
   the signature with the public key a build has compiled in. It prints what a
   client would refuse, if anything.
5. **Publish the GitHub release** — `gh release edit v0.2.0 --draft=false`, or
   the button on its page. The download URLs in the manifest work only from
   this moment.
6. **Publish the manifest.** In `tiamat-updates`, copy `dist/manifest.json`
   and `dist/manifest.json.sig` to `site/test/releases/0.2.0/` and to
   `site/test/`, commit, push. Dokploy redeploys in seconds.
7. **Check it from outside.**

   ```sh
   curl -sO https://updates.tiamatengine.com/test/manifest.json
   curl -sO https://updates.tiamatengine.com/test/manifest.json.sig
   cargo run -p relman -- verify --manifest manifest.json \
     --key "$(grep -v '^#' release-key.pub | tr -d '[:space:]')"
   ```

   Then start an installed copy of the previous version: the front screen
   offers `0.2.0`, downloads it, and the launcher applies it at the next start.
   `tiamat --status` in the install directory says what is installed and what
   is staged.

The order of 5 and 6 matters: a client that sees the manifest before the
release is public gets a 404 from GitHub, reports the download as failed, and
tries again at its next launch. Harmless, and avoidable.

## 8. The download page, on tiamatengine.com

For a person, not for the client, and nothing here affects updates. It should
carry, per release: the four archives (links to the GitHub release are enough),
the version and the commit, and the note that the build is unsigned by the
operating system's standards (`distribution.md` §9: macOS asks once about
quarantine, Windows shows SmartScreen). Install instructions are three lines:
unpack the archive anywhere, run `tiamat` (the launcher), make a shortcut to it.

And because this is a GPLv3 program being distributed as binaries
(`distribution.md` §7): a link to the repository, a link to the **exact commit**
named in the manifest, and a mention that `LICENSE` and `LICENSE.EXCEPTION`
ship inside every archive.

## 9. When it does not work

| The front screen says | What it means |
|---|---|
| nothing about updates at all | The build has no manifest URL or no key: the variable was unset when the tag was built, or this is a working copy. `tiamat --status` shows which. |
| `could not reach …` | DNS, the certificate, or a 404. Run the `curl` lines in §4.4. |
| `the manifest is not signed by a key this build trusts` | The `.sig` is not a signature over the exact bytes served. Something reformatted `manifest.json` after it was signed, or the two head files are from different releases. |
| `signed by an unexpected key` | Signed with a key that is neither the compiled-in one nor the successor the previous manifest committed to. |
| `… is not newer than the installed …` | The version in the manifest is not greater than what is installed. |
| `… sent more than the N bytes it was supposed to` | The host answered with something else — typically a "not found" HTML page sent with status 200. |
| `the download stopped` | The archive's URL did not deliver within its budget. Check the release is published and the URL in the manifest resolves. |

The only thing that has ever needed a re-release is a manifest edited after
signing. Sign last, copy bytes, never open them.
