#!/usr/bin/env python3
# SPDX-FileCopyrightText: Iridesium
# SPDX-License-Identifier: GPL-3.0-only
"""The notice bundle a binary archive has to carry.

Every crate compiled into the shipped binaries — the client, the server and the
launcher, for the target being packaged — has a directory under `licenses/`
holding the licence and notice files its published package contains, copied as
they are. That is what MIT and BSD demand (the copyright and permission notice
preserved), what Apache-2.0 demands (its text and any NOTICE), and what a
reader needs to check any of it. A crate whose package ships no such file gets
the canonical text of each licence it names (from `scripts/licenses/`) and its
authors as the copyright holders, and the index says that is what happened.

`THIRD-PARTY.md` is the index: one line per crate, and where its files are.
`docs/distribution.md` §7 is the policy this implements.

Usage:
  scripts/third-party-notices.py --target <triple> --out <dir> --index <file>
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOTS = ("client", "server", "updater")
FILE_PATTERN = re.compile(r"^(LICENSE|LICENCE|COPYING|COPYRIGHT|NOTICE|UNLICENSE)", re.IGNORECASE)
IDENTIFIER = re.compile(r"[A-Za-z0-9.+-]+")
KEYWORDS = {"OR", "AND", "WITH"}


def metadata(target):
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--filter-platform", target],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return json.loads(out)


def shipped_packages(meta):
    """External packages reachable from the shipped binaries through normal
    dependencies: not build-dependencies, not dev-dependencies, which never
    enter a binary."""
    packages = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    members = set(meta["workspace_members"])
    roots = [pid for pid in members if packages[pid]["name"] in ROOTS]
    if len(roots) != len(ROOTS):
        sys.exit(f"expected the workspace to hold {ROOTS}, found {[packages[r]['name'] for r in roots]}")
    seen = set()
    stack = list(roots)
    while stack:
        pid = stack.pop()
        if pid in seen:
            continue
        seen.add(pid)
        for dep in nodes[pid]["deps"]:
            if any(kind["kind"] is None for kind in dep["dep_kinds"]):
                stack.append(dep["pkg"])
    return sorted(
        (packages[pid] for pid in seen if pid not in members and packages[pid]["source"]),
        key=lambda p: (p["name"], p["version"]),
    )


def licence_ids(expression):
    """The SPDX identifiers named in a licence expression, in order."""
    ids = []
    for token in IDENTIFIER.findall(expression.replace("/", " OR ")):
        if token.upper() in KEYWORDS or token in ids:
            continue
        ids.append(token)
    return ids


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--target", required=True)
    parser.add_argument("--out", required=True, type=Path, help="the licenses/ directory to write")
    parser.add_argument("--index", required=True, type=Path, help="the THIRD-PARTY.md to write")
    parser.add_argument("--texts", type=Path, default=Path("scripts/licenses"))
    parser.add_argument("--version", default="")
    args = parser.parse_args()

    meta = metadata(args.target)
    packages = shipped_packages(meta)
    if args.out.exists():
        shutil.rmtree(args.out)
    args.out.mkdir(parents=True)

    lines = []
    copied = 0
    fallback = 0
    for package in packages:
        name, version = package["name"], package["version"]
        expression = package.get("license") or "(no licence expression declared)"
        source_dir = Path(package["manifest_path"]).parent
        target_dir = args.out / f"{name}-{version}"
        files = sorted(
            entry.name
            for entry in source_dir.iterdir()
            if entry.is_file() and FILE_PATTERN.match(entry.name)
        )
        authors = "; ".join(package.get("authors") or []) or "(no authors listed in the crate)"
        repository = package.get("repository") or "(no repository listed)"
        target_dir.mkdir()
        if files:
            for file in files:
                shutil.copy2(source_dir / file, target_dir / file)
            copied += 1
            where = f"`licenses/{target_dir.name}/` ({', '.join(files)})"
        else:
            # No notice file in the published crate. Include the canonical text
            # of each licence it names, and say who the authors are.
            fallback += 1
            included = []
            for ident in licence_ids(expression):
                text = args.texts / f"{ident}.txt"
                if not text.exists():
                    sys.exit(
                        f"{name} {version} names licence `{ident}` and ships no licence file; "
                        f"add the canonical text as {text} first"
                    )
                shutil.copy2(text, target_dir / f"LICENSE.{ident}.txt")
                included.append(ident)
            (target_dir / "NOTICE.md").write_text(
                f"# {name} {version}\n\n"
                f"The published crate contains no licence or notice file. Its manifest\n"
                f"declares the licence `{expression}`; the canonical text of each licence\n"
                f"named is beside this file. The copyright holders are the crate's authors:\n"
                f"{authors}.\n\nRepository: {repository}\n",
                encoding="utf-8",
            )
            where = (
                f"`licenses/{target_dir.name}/` — the crate ships no licence file; canonical "
                f"text of {', '.join(included)} included, copyright the authors named"
            )
        lines.append(f"- **{name} {version}** — {expression} — {authors} — {where}")

    # The font the client embeds, with its own notice, on the same footing.
    font = Path("crates/client/assets/third-party/go-font")
    font_out = args.out / "go-font"
    font_out.mkdir()
    for file in ("LICENSE", "README.md"):
        shutil.copy2(font / file, font_out / file)

    version = args.version or "(development build)"
    index = [
        "# Third-party notices",
        "",
        f"Tiamat {version} is GPL-3.0-only (LICENSE) with an Additional Permission",
        "(LICENSE.EXCEPTION). The binaries in this archive are built from the crates",
        "listed below, each under the licence its authors chose. **Every crate's own",
        "licence and notice files are in `licenses/<crate>-<version>/`**, copied from",
        "the published package as they are; where a package ships none, the canonical",
        "text of each licence it names is included there instead, with the crate's",
        "authors as the copyright holders, and the entry says so.",
        "",
        "Where a crate offers a choice of licences, the whole expression is shown and",
        "Tiamat takes an option compatible with GPL-3.0-only. Source for any MPL-2.0",
        "crate is available from its repository or from crates.io at the version",
        "named. The corresponding source for Tiamat itself, and for every mod bundled",
        "in `game/`, is named in RELEASE.md.",
        "",
        f"This bundle was produced for the target `{args.target}`; a build for another",
        "target links a slightly different set.",
        "",
        "## Fonts and assets",
        "",
        "- **Go Mono**, embedded in the client — BSD-3-Clause, Copyright 2009 The Go",
        "  Authors — `licenses/go-font/LICENSE`.",
        "- Each mod under `game/` carries its own `LICENSE`, and its own notices for",
        "  any third-party asset it bundles, inside its directory.",
        "",
        f"## Crates ({len(packages)}: {copied} with their own notice files, {fallback} with canonical text)",
        "",
    ] + lines + [""]
    args.index.write_text("\n".join(index), encoding="utf-8")
    print(f"{len(packages)} crates for {args.target}: {copied} copied, {fallback} from canonical text; index {args.index}")


if __name__ == "__main__":
    main()
