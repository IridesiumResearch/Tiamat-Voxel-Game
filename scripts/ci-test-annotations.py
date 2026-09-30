#!/usr/bin/env python3
# SPDX-FileCopyrightText: Iridesium
# SPDX-License-Identifier: GPL-3.0-only
"""Turns a `cargo test` log into one GitHub annotation per failing test.

The job log needs a token to read, but a check run's annotations are served
by the public API (`GET /repos/{owner}/{repo}/check-runs/{job}/annotations`),
so with these a red leg can be diagnosed from anywhere without opening the
log. The title is the test's name and the message is its panic line and the
tail of what it printed — the diagnostics a test writes before it asserts,
which are what every timing flake so far has been found from.

Usage: ci-test-annotations.py <cargo test log>
"""

import re
import sys
from pathlib import Path

ANSI = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")
BLOCK = re.compile(r"^---- (.*) stdout ----$")
PANIC = re.compile(r"panicked at|^thread '.*' panicked")
SUMMARY = re.compile(
    r"^test result: FAILED|^error: test failed|SIGSEGV|SIGABRT|"
    r"error: process didn't exit successfully"
)
# One message may hold this much; GitHub truncates the rest.
MESSAGE_LIMIT = 3500
TAIL_LINES = 40


def escape(text: str) -> str:
    """A workflow command is one line: newlines and percent signs are escaped."""
    return text.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")


def escape_title(text: str) -> str:
    """A title may hold neither colons nor commas."""
    return escape(text).replace(":", "%3A").replace(",", "%2C")


def blocks(lines: list[str]) -> dict[str, list[str]]:
    """Each `---- <name> stdout ----` block, ending at the next block, at the
    `failures:` list or at the result line."""
    found: dict[str, list[str]] = {}
    name = None
    for line in lines:
        head = BLOCK.match(line)
        if head:
            name = head.group(1)
            found.setdefault(name, [])
            continue
        if name is None:
            continue
        if line == "failures:" or line.startswith("test result:"):
            name = None
            continue
        found[name].append(line)
    return found


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: ci-test-annotations.py <cargo test log>", file=sys.stderr)
        return 2
    log = Path(sys.argv[1])
    if not log.is_file():
        print(f"no test log at {log}")
        return 0
    lines = [ANSI.sub("", line.rstrip("\n")) for line in log.open(errors="replace")]

    count = 0
    for name, body in blocks(lines).items():
        panic = next((line for line in body if PANIC.search(line)), "(no panic line)")
        tail = [line for line in body if line.strip()][-TAIL_LINES:]
        message = (panic + "\n\n" + "\n".join(tail))[:MESSAGE_LIMIT]
        print(f"::error title={escape_title(name[:200])}::{escape(message)}")
        count += 1

    # The harness's own summary, for a binary that died without a block: a
    # segfault, a timeout, a missing adapter.
    summary = [line for line in lines if SUMMARY.search(line)][:20]
    if summary:
        print(f"::error title=cargo test summary::{escape(chr(10).join(summary))}")
    print(f"annotated {count} failing test(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
