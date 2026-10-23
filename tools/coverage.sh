#!/bin/sh
# Line coverage of src/ from an instrumented `cargo test`, as CI measures it.
# Needs `rustup component add llvm-tools` and python3. Prints the report from
# tools/coverage.py; with an argument it also writes the badge JSON there.
# Instrumented builds go to target/coverage, apart from the normal ones.
set -eu
out=target/coverage
rm -rf "$out/profraw"
export RUSTFLAGS="-C instrument-coverage" CARGO_TARGET_DIR="$out"
export LLVM_PROFILE_FILE="$PWD/$out/profraw/loginux-%p-%m.profraw"
cargo test --quiet >&2
bin="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin"
objects=$(cargo test --quiet --no-run --message-format=json | python3 -c '
import json, sys
for line in sys.stdin:
    m = json.loads(line)
    if m.get("reason") == "compiler-artifact" and m.get("executable") and m["target"]["name"] == "loginux":
        print("--object", m["executable"])')
"$bin/llvm-profdata" merge -sparse "$out"/profraw/*.profraw -o "$out/loginux.profdata"
# shellcheck disable=SC2086 # one word per --object and path
"$bin/llvm-cov" export --format=lcov --instr-profile="$out/loginux.profdata" --ignore-filename-regex='/(\.cargo|rustc)/' \
    $objects > "$out/lcov.info"
python3 tools/coverage.py "$out/lcov.info" "$@"
