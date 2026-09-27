#!/usr/bin/env python3
"""Print the current benchmark numbers as a table.

`scripts/bench.sh` measures; this reads what it left behind. Criterion writes one
`estimates.json` per benchmark per baseline under `target/criterion`, and this
turns the newest one for each benchmark into a table with the numbers that
actually mean something for an audio engine.

    python3 scripts/bench_table.py            # every group, most recent run
    python3 scripts/bench_table.py engine     # one group
    python3 scripts/bench_table.py --baseline before
    python3 scripts/bench_table.py --all      # including benchmarks nothing ran

The three columns worth reading:

  *mean*        how long one call took.
  *frames/s*    how many frames of audio that is. The device needs one frame per
                frame time, so `headroom` is that ratio: 30x means the callback
                is using about three per cent of its deadline, and 1x means it
                cannot keep up at all. A `bus` row is measured against its own
                rate rather than against 48 kHz, because the same per-frame cost
                at 96 kHz leaves half the headroom.
  *elem/s*      the planner's rows are not frames, so they are reported as the
                cells they plan and no headroom is claimed for them.

There is deliberately no comparison against a recorded number here. Comparing two
*runs* is `critcmp`, and comparing a run against a *fixed threshold* is what this
project does not do — see the measurement discipline in PERFORMANCE.md.
"""

import argparse
import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
CRITERION = ROOT / "target" / "criterion"

# Criterion leaves the numbers for a benchmark behind when it is renamed or
# deleted, and there is no manifest saying which ones a run covered — so a table
# read from the tree alone would quietly report measurements of code that no
# longer exists. `bench.sh` touches this marker immediately before it measures,
# and anything older than it is left out.
MARKER = CRITERION / ".last-run"

# The groups whose throughput is a count of audio frames.
FRAME_GROUPS = {"engine", "rack", "bus", "render"}

# The rate a throughput is measured against when the benchmark does not say.
DEFAULT_RATE = 48_000


def human(nanoseconds):
    if nanoseconds < 1_000:
        return f"{nanoseconds:.1f} ns"
    if nanoseconds < 1_000_000:
        return f"{nanoseconds / 1_000:.2f} us"
    if nanoseconds < 1_000_000_000:
        return f"{nanoseconds / 1_000_000:.3f} ms"
    return f"{nanoseconds / 1_000_000_000:.3f} s"


def newest_mtime(paths):
    """The most recent modification time of a set of paths, or 0."""
    times = []
    for path in paths:
        try:
            times.append(path.stat().st_mtime)
        except OSError:
            pass
    return max(times, default=0.0)


def read_json(path):
    try:
        return json.loads(path.read_text())
    except (OSError, json.JSONDecodeError):
        return None


def throughput_frames(bench_dir):
    """Frames per iteration, if criterion recorded one."""
    for name in ("new", "base"):
        meta = read_json(bench_dir / name / "benchmark.json")
        if meta is None:
            continue
        value = meta.get("throughput")
        if isinstance(value, dict) and value.get("Elements") is not None:
            return float(value["Elements"])
        if isinstance(value, (int, float)):
            return float(value)
    return None


def collect(baseline, group_filter, since):
    """Every benchmark with numbers recorded under `baseline`.

    A benchmark id becomes a *path*: criterion turns a slash in the id into a
    directory, so `bus/48000Hz/512` is three levels deep under the group and the
    group is only the first component.
    """
    rows = []
    for estimates_path in sorted(CRITERION.rglob(f"{baseline}/estimates.json")):
        parts = estimates_path.relative_to(CRITERION).parts
        if len(parts) < 3:
            continue
        group = parts[0]
        bench = "/".join(parts[1:-2])
        if group_filter and group != group_filter:
            continue

        bench_dir = estimates_path.parent.parent
        if since and newest_mtime(bench_dir.rglob("estimates.json")) < since:
            continue

        estimates = read_json(estimates_path)
        if estimates is None or "mean" not in estimates:
            continue
        mean = estimates["mean"]
        rows.append(
            (
                group,
                bench,
                mean["point_estimate"],
                mean["confidence_interval"]["lower_bound"],
                mean["confidence_interval"]["upper_bound"],
                throughput_frames(bench_dir),
            )
        )
    return rows


def headroom_factor(group, bench, frames_per_second):
    """How many times faster than real time a case runs.

    A `bus` row carries its rate in its own name — `48000Hz/512` — so the
    deadline it is measured against is its own, which is the half of the story a
    single 48 kHz column would hide: the same per-frame cost at 96 kHz leaves
    half the headroom.
    """
    if group not in FRAME_GROUPS:
        return None
    rate = DEFAULT_RATE
    for part in bench.split("/"):
        if part.endswith("Hz") and part[:-2].isdigit():
            rate = int(part[:-2])
    return frames_per_second / rate


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("group", nargs="?", help="only this criterion group")
    parser.add_argument(
        "--baseline",
        default="new",
        help="which baseline to read; `new` is the most recent run (default)",
    )
    parser.add_argument(
        "--all",
        action="store_true",
        help="include benchmarks older than the last scripts/bench.sh run",
    )
    args = parser.parse_args()

    if not CRITERION.exists():
        print(f"no {CRITERION.relative_to(ROOT)}; run scripts/bench.sh first", file=sys.stderr)
        return 1

    # A named baseline was written by whichever run saved it and may well predate
    # the marker, so only the newest run is filtered.
    since = 0.0
    if not args.all and args.baseline == "new" and MARKER.exists():
        since = MARKER.stat().st_mtime

    rows = collect(args.baseline, args.group, since)
    if not rows:
        where = f" in group {args.group!r}" if args.group else ""
        print(f"nothing recorded{where} under baseline {args.baseline!r}", file=sys.stderr)
        return 1

    rows.sort()
    width = max(len(f"{group}/{bench}") for group, bench, *_ in rows)
    frame_rows = any(group in FRAME_GROUPS for group, *_ in rows)
    header = f"{'benchmark':<{width}}  {'mean':>10}  {'95% interval':>23}"
    if any(elements for *_, elements in rows):
        header += f"  {'frames/s' if frame_rows else 'elem/s':>11}"
    if frame_rows:
        header += f"  {'headroom':>8}"
    print(header)
    print("-" * len(header))

    for group, bench, mean, low, high, elements in rows:
        line = (
            f"{group + '/' + bench:<{width}}  {human(mean):>10}  "
            f"{human(low) + ' .. ' + human(high):>23}"
        )
        if elements:
            per_second = elements / (mean / 1e9)
            line += f"  {per_second:>11,.0f}"
            factor = headroom_factor(group, bench, per_second)
            if factor is not None:
                line += f"  {factor:>7.1f}x"
        print(line)

    if since:
        import time

        print(f"\nmeasured {time.time() - since:.0f} s ago; --all for every run ever recorded")
    return 0


if __name__ == "__main__":
    sys.exit(main())
