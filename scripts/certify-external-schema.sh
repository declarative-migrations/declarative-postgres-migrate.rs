#!/usr/bin/env bash
set -euo pipefail

umask 077

usage() {
  cat >&2 <<'USAGE'
usage: certify-external-schema.sh \
  --dpm <path> \
  --source-sql <path> \
  --target <postgres-url> \
  --shadow <postgres-url> \
  --output-dir <path> \
  [--allow-destructive-sql]

Snapshots the live target without writing to it, plans the migration from an
externally owned SQL file, verifies convergence on a throwaway shadow database,
and writes bounded evidence. The caller remains responsible for materializing
the immutable source package and proving the SQL file's package provenance.
USAGE
  exit 64
}

fail() {
  printf '%s\n' "$1" >&2
  exit 2
}

sha256_file() {
  local path="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$path" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$path" | awk '{print $1}'
  else
    fail "neither sha256sum nor shasum is available"
  fi
}

dpm=""
source_sql=""
target=""
shadow=""
output_dir=""
allow_destructive_sql="false"

while (( $# > 0 )); do
  case "$1" in
    --dpm) dpm="${2:-}"; shift 2 ;;
    --source-sql) source_sql="${2:-}"; shift 2 ;;
    --target) target="${2:-}"; shift 2 ;;
    --shadow) shadow="${2:-}"; shift 2 ;;
    --output-dir) output_dir="${2:-}"; shift 2 ;;
    --allow-destructive-sql) allow_destructive_sql="true"; shift ;;
    *) usage ;;
  esac
done

[[ -n "$dpm" && -n "$source_sql" && -n "$target" && -n "$shadow" && -n "$output_dir" ]] || usage
[[ -x "$dpm" ]] || fail "dpm binary is not executable: $dpm"
[[ -f "$source_sql" && ! -L "$source_sql" ]] || \
  fail "source SQL must be a regular non-symlink file: $source_sql"
[[ "$target" == postgres://* || "$target" == postgresql://* ]] || \
  fail "target must use postgres:// or postgresql://"
[[ "$shadow" == postgres://* || "$shadow" == postgresql://* ]] || \
  fail "shadow must use postgres:// or postgresql://"
[[ "$target" != "$shadow" ]] || fail "target and shadow databases must be distinct"

source_bytes="$(wc -c < "$source_sql" | tr -d '[:space:]')"
[[ "$source_bytes" =~ ^[0-9]+$ ]] || fail "could not determine source SQL size"
(( source_bytes > 0 && source_bytes <= 16777216 )) || \
  fail "source SQL must be between 1 and 16777216 bytes"

if [[ -e "$output_dir" || -L "$output_dir" ]]; then
  [[ -d "$output_dir" && ! -L "$output_dir" ]] || \
    fail "output directory must be a real directory, not a symlink: $output_dir"
else
  mkdir -p -- "$output_dir"
fi
chmod 700 "$output_dir"

target_catalog="$output_dir/target-catalog.json"
plan="$output_dir/migration-plan.json"
verify_log="$output_dir/dpm-verify.txt"
summary="$output_dir/external-schema-certification.json"

for artifact in "$target_catalog" "$plan" "$verify_log" "$summary"; do
  [[ ! -e "$artifact" && ! -L "$artifact" ]] || \
    fail "refusing to overwrite existing evidence artifact: $artifact"
done

# This is the only command that connects to the live target. `dpm dump` only
# introspects catalogs; all planning and replay below use this immutable file.
"$dpm" dump \
  --source "$target" \
  --out "$target_catalog"

if [[ "$allow_destructive_sql" == "true" ]]; then
  "$dpm" diff \
    --source-sql "$source_sql" \
    --target-json "$target_catalog" \
    --shadow "$shadow" \
    --format json \
    --out "$plan" \
    --allow-destructive-sql
  "$dpm" verify \
    --source-sql "$source_sql" \
    --target-json "$target_catalog" \
    --shadow "$shadow" \
    --allow-destructive-sql \
    > "$verify_log" 2>&1
else
  "$dpm" diff \
    --source-sql "$source_sql" \
    --target-json "$target_catalog" \
    --shadow "$shadow" \
    --format json \
    --out "$plan"
  "$dpm" verify \
    --source-sql "$source_sql" \
    --target-json "$target_catalog" \
    --shadow "$shadow" \
    > "$verify_log" 2>&1
fi

source_sha="$(sha256_file "$source_sql")"
dpm_sha="$(sha256_file "$dpm")"
target_catalog_sha="$(sha256_file "$target_catalog")"
plan_sha="$(sha256_file "$plan")"
verify_sha="$(sha256_file "$verify_log")"
dpm_version="$($dpm --version | head -n 1 | tr -d '\r')"

python3 - \
  "$summary" \
  "$plan" \
  "$source_sha" \
  "$source_bytes" \
  "$dpm_sha" \
  "$dpm_version" \
  "$target_catalog_sha" \
  "$plan_sha" \
  "$verify_sha" \
  "$allow_destructive_sql" <<'PY'
import hashlib
import json
import pathlib
import re
import sys

(
    output,
    plan_path,
    source_sha,
    source_bytes,
    dpm_sha,
    dpm_version,
    target_catalog_sha,
    plan_sha,
    verify_sha,
    allow_destructive_sql,
) = sys.argv[1:]

plan = json.loads(pathlib.Path(plan_path).read_text(encoding="utf-8"))
plan_checksum = plan.get("planChecksum")
if not isinstance(plan_checksum, str) or not re.fullmatch(r"[0-9a-fA-F]{16}", plan_checksum):
    raise SystemExit("migration plan has no valid reviewed plan checksum")
summary = plan.get("summary")
if not isinstance(summary, dict):
    raise SystemExit("migration plan has no summary")
for key in ("total", "destructive", "gated", "manual"):
    if type(summary.get(key)) is not int or summary[key] < 0:
        raise SystemExit(f"migration plan summary has invalid {key!r}")

identity_payload = "\n".join(
    (
        "external-schema-read-only-plan/v1",
        source_sha,
        target_catalog_sha,
        dpm_sha,
        plan_checksum.lower(),
        plan_sha,
        verify_sha,
    )
).encode("utf-8")

report = {
    "version": 1,
    "evidenceClass": "external-schema-read-only-plan-certification",
    "decisionEligible": False,
    "reviewRequired": True,
    "sourceSqlSha256": source_sha,
    "sourceSqlBytes": int(source_bytes),
    "targetCatalogSha256": target_catalog_sha,
    "dpmSha256": dpm_sha,
    "dpmVersion": dpm_version,
    "planSha256": plan_sha,
    "planChecksum": plan_checksum.lower(),
    "planSummary": summary,
    "verifyLogSha256": verify_sha,
    "evidenceDigest": hashlib.sha256(identity_payload).hexdigest(),
    "destructiveSqlEnabled": allow_destructive_sql == "true",
    "targetSnapshottedReadOnly": True,
    "applied": False,
    "verified": True,
    "warning": (
        "No database URL or credential is recorded. Package coordinate, lockfile, "
        "artifact digest, and source commit remain caller-owned provenance."
    ),
}
pathlib.Path(output).write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
PY

chmod 600 "$target_catalog" "$plan" "$verify_log" "$summary"
printf 'external schema planned and verified without target writes; report=%s\n' "$summary"
