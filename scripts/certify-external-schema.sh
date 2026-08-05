#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
usage: certify-external-schema.sh \
  --dpm <path> \
  --source-sql <path> \
  --target <postgres-url> \
  --shadow <postgres-url> \
  --output-dir <path>

Runs `dpm apply` followed by `dpm verify` for an externally owned declarative
schema and writes bounded, non-secret evidence. The caller remains responsible
for fetching and authenticating the external schema source.
USAGE
  exit 64
}

dpm=""
source_sql=""
target=""
shadow=""
output_dir=""

while (( $# > 0 )); do
  case "$1" in
    --dpm) dpm="${2:-}"; shift 2 ;;
    --source-sql) source_sql="${2:-}"; shift 2 ;;
    --target) target="${2:-}"; shift 2 ;;
    --shadow) shadow="${2:-}"; shift 2 ;;
    --output-dir) output_dir="${2:-}"; shift 2 ;;
    *) usage ;;
  esac
done

[[ -n "$dpm" && -n "$source_sql" && -n "$target" && -n "$shadow" && -n "$output_dir" ]] || usage
[[ -x "$dpm" ]] || { echo "dpm binary is not executable: $dpm" >&2; exit 2; }
[[ -f "$source_sql" && ! -L "$source_sql" ]] || {
  echo "source SQL must be a regular non-symlink file: $source_sql" >&2
  exit 2
}
[[ "$target" == postgres://* || "$target" == postgresql://* ]] || {
  echo "target must use postgres:// or postgresql://" >&2
  exit 2
}
[[ "$shadow" == postgres://* || "$shadow" == postgresql://* ]] || {
  echo "shadow must use postgres:// or postgresql://" >&2
  exit 2
}
[[ "$target" != "$shadow" ]] || {
  echo "target and shadow databases must be distinct" >&2
  exit 2
}

source_bytes="$(wc -c < "$source_sql" | tr -d '[:space:]')"
[[ "$source_bytes" =~ ^[0-9]+$ ]] || { echo "could not determine source SQL size" >&2; exit 2; }
(( source_bytes > 0 && source_bytes <= 16777216 )) || {
  echo "source SQL must be between 1 and 16777216 bytes" >&2
  exit 2
}

mkdir -p "$output_dir"
chmod 700 "$output_dir"
apply_log="$output_dir/dpm-apply.txt"
verify_log="$output_dir/dpm-verify.txt"
summary="$output_dir/external-schema-certification.json"

"$dpm" apply \
  --source-sql "$source_sql" \
  --target "$target" \
  --shadow "$shadow" \
  --yes \
  > "$apply_log"

"$dpm" verify \
  --source-sql "$source_sql" \
  --target "$target" \
  --shadow "$shadow" \
  > "$verify_log"

source_sha="$(sha256sum "$source_sql" | awk '{print $1}')"
dpm_sha="$(sha256sum "$dpm" | awk '{print $1}')"
apply_sha="$(sha256sum "$apply_log" | awk '{print $1}')"
verify_sha="$(sha256sum "$verify_log" | awk '{print $1}')"
dpm_version="$($dpm --version | head -n 1 | tr -d '\r')"

python3 - \
  "$summary" \
  "$source_sha" \
  "$source_bytes" \
  "$dpm_sha" \
  "$dpm_version" \
  "$apply_sha" \
  "$verify_sha" <<'PY'
import json
import pathlib
import sys

output, source_sha, source_bytes, dpm_sha, dpm_version, apply_sha, verify_sha = sys.argv[1:]
report = {
    "version": 1,
    "evidenceClass": "external-schema-local-certification",
    "decisionEligible": False,
    "sourceSqlSha256": source_sha,
    "sourceSqlBytes": int(source_bytes),
    "dpmSha256": dpm_sha,
    "dpmVersion": dpm_version,
    "applyLogSha256": apply_sha,
    "verifyLogSha256": verify_sha,
    "applied": True,
    "verified": True,
    "warning": "No database URLs or credentials are recorded. Repository identity and source commit remain caller-owned evidence.",
}
pathlib.Path(output).write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
PY

chmod 600 "$apply_log" "$verify_log" "$summary"
printf 'external schema applied and verified; report=%s\n' "$summary"
