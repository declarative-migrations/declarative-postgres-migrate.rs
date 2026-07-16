#!/usr/bin/env bash
# Run the CockroachDB integration suite against an isolated single-node
# cluster. Override DPM_COCKROACH_IMAGE to exercise a supported server image.
set -euo pipefail
cd "$(dirname "$0")/.."

container="dpm-cockroach-test-$$"
port="${DPM_COCKROACH_PORT:-26258}"
image="${DPM_COCKROACH_IMAGE:-cockroachdb/cockroach:v25.2.4}"

cleanup() {
  docker stop "$container" >/dev/null 2>&1 || true
}
trap cleanup EXIT

docker run --rm -d --name "$container" -p "$port:26257" "$image" start-single-node --insecure >/dev/null
for _ in $(seq 1 30); do
  if docker exec "$container" cockroach sql --insecure --host=localhost:26257 --execute='SELECT 1' >/dev/null 2>&1; then
    DPM_TEST_COCKROACH_DATABASE_URL="postgresql://root@localhost:$port/defaultdb?sslmode=disable" \
      cargo test --test cockroach "$@"
    exit 0
  fi
  sleep 1
done

echo "CockroachDB did not become ready" >&2
exit 1
