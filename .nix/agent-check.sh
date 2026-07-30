#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$repo_root" || ! -f "$repo_root/Cargo.toml" ]]; then
  echo "agent-check must run from the dpm Git worktree" >&2
  exit 1
fi
cd "$repo_root"

cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --lib
cargo test --locked --test fuzz_splitter
bash scripts/check-release-contract.sh
