#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Stop a cache restore before it fills the disk, and say what filled it.

Why this exists: when a restore fills the runner's disk, the runner cannot write
its own log and dies with `No space left on device`. GitHub then keeps no log for
the job, so the free space it reported and the name of what filled the disk are
both lost.

`watch` runs in the background during the restore. If free space drops below the
limit, it records that, stops the extracting process while there is still room
to work, and writes a report of what is on the disk. It keeps watching until told
to finish, because the restore downloads before it extracts: a trip during the
download finds nothing to stop yet.

`check` runs after a restore and fails the job with the report if the guard
tripped, because a partly extracted cache must not be built on. `finish` does the
same and also ends the watcher.
"""

from __future__ import annotations

import argparse
import heapq
import os
import shutil
import subprocess
import sys
import time
from collections import defaultdict
from collections.abc import Callable
from pathlib import Path

GIB = 1024**3
# `check` may run while `watch` is still scanning for its report. Bounded so a
# watcher that died mid-scan still fails the job, just without the report.
REPORT_WAIT_SECONDS = 300.0
TOP_DIRECTORIES = 25
TOP_FILES = 15
# Directories deeper than this are added to their ancestor at this depth.
SUMMARY_DEPTH = 3


def scan(root: Path) -> tuple[dict[str, list[int]], list[tuple[int, int, str]]]:
    """Return [apparent, allocated] bytes per directory, and the largest files.

    Both sizes are kept because they tell different stories: a sparse file is
    large by apparent size only.
    """
    directories: dict[str, list[int]] = defaultdict(lambda: [0, 0])
    largest: list[tuple[int, int, str]] = []
    seen: set[tuple[int, int]] = set()
    base_depth = len(root.parts)

    for current, subdirs, names in os.walk(root, followlinks=False):
        path = Path(current)
        depth = len(path.parts) - base_depth
        bucket = path if depth <= SUMMARY_DEPTH else Path(*path.parts[: base_depth + SUMMARY_DEPTH])
        for name in names:
            full = path / name
            try:
                info = os.lstat(full)
            except OSError:
                continue  # Removed while scanning; the extractor may still be exiting.
            # Hard links take their space once.
            if (info.st_dev, info.st_ino) in seen:
                continue
            seen.add((info.st_dev, info.st_ino))
            allocated = info.st_blocks * 512
            totals = directories[str(bucket)]
            totals[0] += info.st_size
            totals[1] += allocated
            entry = (info.st_size, allocated, str(full))
            if len(largest) < TOP_FILES:
                heapq.heappush(largest, entry)
            else:
                heapq.heappushpop(largest, entry)
    return dict(directories), sorted(largest, reverse=True)


def gib(size: int) -> str:
    return f"{size / GIB:8.2f} GiB"


def build_report(path: Path, inspect: list[Path], limit: int, free: int, total: int) -> str:
    lines = [
        f"Free space on {path} fell to {free / GIB:.2f} GiB of {total / GIB:.2f} GiB, "
        f"below the {limit / GIB:.2f} GiB limit, so the guard stopped the restore.",
    ]
    for root in inspect:
        if not root.exists():
            lines += ["", f"{root}: does not exist"]
            continue
        directories, largest = scan(root)
        apparent = sum(sizes[0] for sizes in directories.values())
        allocated = sum(sizes[1] for sizes in directories.values())
        lines += ["", f"{root}: {gib(apparent).strip()} apparent, {gib(allocated).strip()} allocated"]
        lines += ["  Largest directories (apparent, allocated):"]
        ranked = sorted(directories.items(), key=lambda item: item[1][1], reverse=True)
        lines += [f"  {gib(a)} {gib(b)}  {name}" for name, (a, b) in ranked[:TOP_DIRECTORIES]]
        lines += ["  Largest files (apparent, allocated):"]
        lines += [f"  {gib(a)} {gib(b)}  {name}" for a, b, name in largest]
    return "\n".join(lines) + "\n"


def stop_process(name: str) -> None:
    subprocess.run(["pkill", "-x", name], check=False)


def watch(
    path: Path,
    limit: int,
    process: str,
    report: Path,
    stop_file: Path,
    inspect: list[Path],
    interval: float,
    usage: Callable[[Path], tuple[int, int]],
    stop: Callable[[str], None] = stop_process,
    sleep: Callable[[float], None] = time.sleep,
) -> bool:
    """Watch until told to stop. Returns whether the guard tripped."""
    tripped = False
    while not stop_file.exists():
        free, total = usage(path)
        if free < limit:
            # First, so `check` cannot pass between stopping the process and the
            # report being written.
            if not tripped:
                trip_marker(report).touch()
            stop(process)
            if not tripped:
                tripped = True
                write_report(report, build_report(path, inspect, limit, free, total))
        sleep(interval)
    return tripped


def trip_marker(report: Path) -> Path:
    return report.with_name(report.name + ".tripped")


def write_report(report: Path, text: str) -> None:
    # Temporary name: `check` must never print half a report.
    partial = report.with_name(report.name + ".partial")
    partial.write_text(text)
    partial.replace(report)


def disk_usage(path: Path) -> tuple[int, int]:
    usage = shutil.disk_usage(path)
    return usage.free, usage.total


def check(
    path: Path,
    report: Path,
    wait: float = REPORT_WAIT_SECONDS,
    sleep: Callable[[float], None] = time.sleep,
    clock: Callable[[], float] = time.monotonic,
) -> int:
    free, total = disk_usage(path)
    print(f"Free space on {path} after the restore: {free / GIB:.2f} GiB of {total / GIB:.2f} GiB")
    if not trip_marker(report).exists():
        return 0
    deadline = clock() + wait
    while not report.exists() and clock() < deadline:
        sleep(1.0)
    if report.exists():
        print(report.read_text())
    else:
        print(f"The guard tripped but wrote no report within {wait:.0f} seconds.")
    print("::error::The cache restore was stopped because the disk was nearly full.")
    return 1


def finish(path: Path, report: Path, stop_file: Path, **options) -> int:
    # Checked first: the watcher must not stop while a restore could still trip it.
    code = check(path, report, **options)
    stop_file.touch()
    return code


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("watch", "check", "finish"):
        command = commands.add_parser(name)
        command.add_argument("--path", type=Path, default=Path("/"))
        command.add_argument("--report", type=Path, required=True)
        if name != "check":
            command.add_argument("--stop-file", type=Path, required=True)
        if name == "watch":
            # A healthy restore extracts about 0.3 GiB/s, and data that compresses
            # very well can go faster. This leaves room to notice and stop it
            # between checks, to scan, and for the runner's own log.
            command.add_argument("--min-free-gib", type=float, default=6.0)
            command.add_argument("--process", default="tar")
            command.add_argument("--inspect", type=Path, action="append", default=[])
            command.add_argument("--interval", type=float, default=2.0)
    args = parser.parse_args(argv)

    if args.command == "check":
        return check(args.path, args.report)
    if args.command == "finish":
        return finish(args.path, args.report, args.stop_file)
    watch(
        args.path,
        int(args.min_free_gib * GIB),
        args.process,
        args.report,
        args.stop_file,
        args.inspect,
        args.interval,
        disk_usage,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
