#!/usr/bin/env bash
# Mirror the GitHub CI gate locally before pushing, so fmt / prettier / clippy
# failures are caught here instead of on the PR.
#
#   ./scripts/ci-check.sh
#
# Covers the same checks CI enforces: cargo fmt --check, clippy (via cargo-xwin),
# cargo test, prettier --check, svelte-check, vite build, and gitleaks.
#
# IMPORTANT: always re-run this AFTER `cargo fmt` -- reformatting can grow a
# function past clippy::too_many_lines, which fmt alone will not report.
set -uo pipefail
cd "$(dirname "$0")/.."
TARGET=x86_64-pc-windows-msvc
fail=0
step() {
	local name="$1"
	shift
	echo
	echo "== $name =="
	"$@" || {
		echo "!! FAILED: $name"
		fail=1
	}
}

step "cargo fmt --all --check" cargo fmt --all --check
step "clippy (xwin)" cargo xwin clippy -p launcher --target "$TARGET" -- -D warnings
step "cargo test" cargo test -p launcher
step "prettier --check" bash -c "cd crates/launcher && npx prettier --check ."
step "svelte-check" bash -c "cd crates/launcher && node node_modules/svelte-check/bin/svelte-check --tsconfig ./tsconfig.json"
step "vite build" bash -c "cd crates/launcher && node node_modules/vite/bin/vite.js build"
command -v gitleaks >/dev/null 2>&1 && step "gitleaks" gitleaks detect --source . --no-banner --redact

echo
if [ "$fail" -eq 0 ]; then
	echo "ALL GREEN -- safe to push"
else
	echo "FAILURES ABOVE -- fix before pushing"
fi
exit "$fail"
