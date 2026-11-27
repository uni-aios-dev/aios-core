#!/usr/bin/env python3
"""Require a SAFETY comment for every newly added unsafe block.

Existing unsafe code is grandfathered; only added lines in a diff are checked.
Usage: python3 scripts/check_unsafe_safety.py [base-ref]
"""

from __future__ import annotations

import re
import subprocess
import sys

WINDOW = 6
UNSAFE = re.compile(r"\bunsafe\b")


def main() -> int:
    base = sys.argv[1] if len(sys.argv) > 1 else "HEAD~1"
    result = subprocess.run(
        ["git", "diff", "--unified=0", base, "--", "*.rs"],
        text=True,
        capture_output=True,
    )
    if result.returncode != 0:
        print(f"Cannot diff against {base}: {result.stderr.strip()}")
        return 2

    violations: list[str] = []
    file_name = ""
    added: list[str] = []
    line_no = 0

    def flush() -> None:
        for index, line in enumerate(added):
            if not UNSAFE.search(line):
                continue
            if line.lstrip().startswith("///") or line.lstrip().startswith("//!"):
                continue
            window = added[max(0, index - WINDOW) : index]
            if any("SAFETY:" in previous for previous in window):
                continue
            violations.append(f"{file_name}:{line_no - len(added) + index + 1}: {line.strip()}")

    for raw in result.stdout.splitlines():
        if raw.startswith("+++ b/"):
            flush()
            file_name = raw[6:]
            added = []
        elif raw.startswith("@@"):
            flush()
            added = []
            match = re.search(r"\+(\d+)", raw)
            line_no = int(match.group(1)) if match else 0
        elif raw.startswith("+") and not raw.startswith("+++"):
            added.append(raw[1:])
            line_no += 1
        elif raw.startswith("-") and not raw.startswith("---"):
            continue
        elif not raw.startswith("\\"):
            flush()
            added = []
    flush()

    if violations:
        print("New unsafe blocks require a nearby `// SAFETY:` comment:")
        for violation in violations:
            print(f"  {violation}")
        return 1
    print("No undocumented new unsafe blocks.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
