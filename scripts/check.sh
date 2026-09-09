#!/usr/bin/env sh
# The CI gate, runnable locally. Set TEST_DATABASE_URL (database name ending in
# _test) to also run the PostgreSQL acceptance tests.
set -eu
cd "$(dirname "$0")/.."

cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace
cargo test --workspace

if [ -n "${TEST_DATABASE_URL:-}" ]; then
  cargo test -p ops-persistence -- --ignored
else
  echo "TEST_DATABASE_URL unset: skipped the PostgreSQL acceptance tests"
fi

( cd web && npm ci --no-audit --no-fund && npm run typecheck && npm run build )
echo "all checks passed"
