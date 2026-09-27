#!/usr/bin/env bash
#
# Everything that has to be true before a change is finished.
#
#   scripts/check.sh            # the gate: about a minute in a debug build
#   scripts/check.sh --load     # and the soak and the deep fuzz on top
#
# The distinction the whole performance story is built on: this script is the
# *deterministic* half. Every check below either passes or fails, on any machine,
# at any speed. The wall-clock half is `scripts/bench.sh`, and it never fails.
#
# What is checked, and why each one is here:
#
#   cargo test                  the whole suite: 980 unit tests, 21 offline
#                               renders of the real audio path, 10 load tests,
#                               and the four that assert the audio callback
#                               allocates nothing. The golden hashes inside the
#                               render suite are what make an optimisation
#                               checkable — a change that moves a sample fails
#                               here, and that is the point.
#   cargo clippy                warnings are errors, because a warning nobody
#                               reads is a warning nobody acts on.
#   cargo test --release        the same assertions in the profile that ships.
#                               Some of them mean different things there: the
#                               cost-per-frame comparison in the soak is only
#                               meaningful with an optimiser, and the allocation
#                               counter is thread-local and profile-independent.
#
# With --load, two more that are measured in minutes rather than seconds:
#
#   the soak                    sixty seconds of the worst case, asserting it
#                               does not grow, does not drift, costs no more at
#                               the end than at the start, and allocates nothing.
#   the deep fuzz               sixty thousand mutated files against every
#                               parser in the crate.

set -euo pipefail

cd "$(dirname "$0")/.."

load=0
if [ "${1:-}" = "--load" ]; then
    load=1
fi

step() {
    echo
    echo "==> $*"
}

step "cargo test"
cargo test

step "cargo clippy --all-targets"
cargo clippy --all-targets -- -D warnings

step "cargo test --release"
cargo test --release

if [ "$load" = "1" ]; then
    step "cargo test --release --test stress -- --ignored (the soak and the deep fuzz)"
    cargo test --release --test stress -- --ignored --nocapture
fi

echo
echo "all checks passed"
if [ "$load" != "1" ]; then
    echo "(scripts/check.sh --load also runs the soak and the deep fuzz)"
fi
