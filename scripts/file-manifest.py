#!/usr/bin/env python3
# SPDX-FileCopyrightText: Iridesium
# SPDX-License-Identifier: GPL-3.0-only
"""Every file under a directory with its SHA-256, one per line, sorted.

The manifest of an archive's contents — `docs/distribution.md` §7 — written
last by the packager and checked by scripts/check-archive.sh. The manifest
file itself is left out, since it cannot contain its own hash.

Usage: scripts/file-manifest.py <root> <out>      (paths relative to <root>)
       scripts/file-manifest.py --check <root> <manifest>
"""

import hashlib
import sys
from pathlib import Path


def entries(root):
    for path in sorted(p for p in root.rglob("*") if p.is_file() or p.is_symlink()):
        if path.is_symlink():
            sys.exit(f"{path} is a symlink; an archive carries files, not links")
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        yield digest, path.relative_to(root).as_posix()


def main(argv):
    if argv[:1] == ["--check"]:
        root, manifest = Path(argv[1]), Path(argv[2])
        listed = {}
        for line in manifest.read_text(encoding="utf-8").splitlines():
            if line.strip():
                digest, name = line.split("  ", 1)
                listed[name] = digest
        actual = {name: digest for digest, name in entries(root) if name != manifest.relative_to(root).as_posix()}
        bad = [n for n in listed if actual.get(n) != listed[n]]
        extra = [n for n in actual if n not in listed]
        for name in bad:
            print(f"FAIL {name}: {'missing' if name not in actual else 'hash differs'}")
        for name in extra:
            print(f"FAIL {name}: in the archive but not in MANIFEST.txt")
        if bad or extra:
            sys.exit(1)
        print(f"MANIFEST.txt: {len(listed)} files, all present and matching")
        return
    root, out = Path(argv[0]), Path(argv[1])
    lines = [f"{digest}  {name}" for digest, name in entries(root) if root / name != out]
    out.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"{out}: {len(lines)} files")


if __name__ == "__main__":
    main(sys.argv[1:])
