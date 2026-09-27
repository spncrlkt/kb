#!/usr/bin/env bash
#
# Measure the audio path and record the result under a name.
#
#   scripts/bench.sh                 # measure, record as "current"
#   scripts/bench.sh before          # measure, record as "before"
#   ... make a change ...
#   scripts/bench.sh after
#   critcmp before after             # cargo install critcmp
#
# Criterion writes an estimate per benchmark into target/criterion, and a
# baseline is just a named copy of that; `critcmp` prints the two side by side
# with the change as a percentage. There is deliberately no threshold anywhere in
# this: a wall-clock number on a laptop is a report, not a pass or a fail, and a
# gate built on one either never fires or fires on the weather.
#
# The deterministic gates are elsewhere:
#
#   cargo test                       # correct, and still the same sound
#   cargo test --test allocation     # and still no allocation on the audio thread
#
# Anything passed after the name goes to criterion: `scripts/bench.sh run --quick`
# for a fast rough pass, `scripts/bench.sh run 'engine/'` for one group.

set -euo pipefail

cd "$(dirname "$0")/.."

name="${1:-current}"
shift || true

# Marked before the measuring rather than after it, so `bench_table.py` can tell
# this run's benchmarks from the ones a previous layout left behind. Criterion
# has no manifest, and a table that silently includes a benchmark of code that
# has since been renamed is worse than no table.
mkdir -p target/criterion
touch target/criterion/.last-run

echo "==> cargo bench --bench audio -- --save-baseline $name $*"
cargo bench --bench audio -- --save-baseline "$name" "$@"

echo
python3 scripts/bench_table.py || true

cat <<EOF

recorded as '$name'
  compare two runs:  critcmp <other> $name      (cargo install critcmp)
  one group's raw numbers:  python3 scripts/bench_table.py engine
  everything ever measured: python3 scripts/bench_table.py --all
EOF
