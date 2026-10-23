#!/usr/bin/env python3
"""Line coverage of Loginux's own code, from an LCOV file.

    python3 tools/coverage.py lcov.info [badge.json]

Counts the lines of src/*.rs and leaves the test code out: the test helpers
(fake.rs, testenv.rs) and each file's `#[cfg(test)] mod tests` block, which
sits at the end of the file. Lines marked with lcov's own markers are left out
too, and counted: LCOV_EXCL_LINE on a line, or every line from LCOV_EXCL_START
to LCOV_EXCL_STOP. Prints a line per file and the total. With a second argument
it also writes a shields.io endpoint badge. Standard library only.
"""
import json
import math
import sys
from pathlib import Path

SRC = Path(__file__).resolve().parent.parent / "src"
TEST_HELPERS = {"fake.rs", "testenv.rs"}


def test_module_start(path):
    """Line number of the `#[cfg(test)]` that opens the trailing test module, or None."""
    lines = Path(path).read_text().splitlines()
    for i, line in enumerate(lines[:-1]):
        if line.strip() == "#[cfg(test)]" and lines[i + 1].startswith("mod tests"):
            return i + 1
    return None


def percent(hit, count):
    """hit/count as a percentage rounded down to one decimal, so 100.0 means every line."""
    return math.floor(1000 * hit / count) / 10 if count else 100.0


def excluded(path):
    """Line numbers the LCOV_EXCL markers take out of the count."""
    out, inside = set(), False
    for number, line in enumerate(Path(path).read_text().splitlines(), 1):
        inside = inside or "LCOV_EXCL_START" in line
        if inside or "LCOV_EXCL_LINE" in line:
            out.add(number)
        inside = inside and "LCOV_EXCL_STOP" not in line
    return out


def read_lcov(path):
    """{source file: {line: hits}} from an LCOV trace."""
    files, current = {}, None
    for line in Path(path).read_text().splitlines():
        if line.startswith("SF:"):
            current = files.setdefault(line[3:], {})
        elif line.startswith("DA:") and current is not None:
            number, hits = line[3:].split(",")[:2]
            current[int(number)] = max(current.get(int(number), 0), int(hits))
        elif line == "end_of_record":
            current = None
    return files


def main():
    rows, total, covered, marked = [], 0, 0, 0
    for path, lines in sorted(read_lcov(sys.argv[1]).items()):
        p = Path(path)
        if p.resolve().parent != SRC or p.name in TEST_HELPERS:
            continue  # a dependency's source, or a test helper
        cut, out = test_module_start(path), excluded(path)
        code = {number: hits for number, hits in lines.items() if cut is None or number < cut}
        marked += sum(1 for number in code if number in out)
        counted = [hits for number, hits in code.items() if number not in out]
        hit = sum(1 for h in counted if h > 0)
        rows.append((p.name, hit, len(counted)))
        total += len(counted)
        covered += hit
    if total == 0:
        sys.exit(f"{sys.argv[1]} has no lines of {SRC}: the trace and this checkout do not match")
    for name, hit, count in rows:
        print(f"{name:12} {hit:5} / {count:<5} {percent(hit, count):5.1f}%")
    total_percent = percent(covered, total)
    print(f"{'total':12} {covered:5} / {total:<5} {total_percent:5.1f}%")
    print(f"{marked} lines left out by LCOV_EXCL markers")
    if len(sys.argv) > 2:
        p = total_percent
        colour = "brightgreen" if p >= 80 else "green" if p >= 60 else "yellow" if p >= 40 else "orange"
        message = "100%" if covered == total else f"{p:.1f}%"
        badge = {"schemaVersion": 1, "label": "coverage", "message": message, "color": colour}
        Path(sys.argv[2]).write_text(json.dumps(badge) + "\n")


if __name__ == "__main__":
    main()
