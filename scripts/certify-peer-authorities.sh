#!/usr/bin/env bash
set -euo pipefail

umask 077

usage() {
  cat >&2 <<'USAGE'
usage: certify-peer-authorities.sh \
  --dpm <path> \
  --typespec-sql <path> \
  --json-schema-sql <path> \
  --typespec-types <path> \
  --json-schema-types <path> \
  --seaorm-projection <path> \
  --diesel-projection <path> \
  --shadow <postgres-url> \
  --output-dir <path>

Compares the independently generated TypeSpec and JSON Schema/OpenAPI SQL and
type artifacts, then compares SeaORM and Diesel projection manifests. Any
missing artifact, tool error, or semantic discrepancy writes a PAUSE
certificate and exits 3. The script never selects an authority automatically.

Type manifests must use format declmig.generated-types/v1. ORM manifests must
use format declmig.orm-projection/v1. Each manifest carries provenance beside a
required semanticModel object; only semanticModel participates in equality.
USAGE
  exit 64
}

fatal() {
  printf 'peer-authority certification error: %s\n' "$1" >&2
  exit 2
}

sha256_file() {
  local path="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$path" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$path" | awk '{print $1}'
  else
    fatal "neither sha256sum nor shasum is available"
  fi
}

sha256_optional() {
  local path="$1"
  if [[ -f "$path" && ! -L "$path" ]]; then
    sha256_file "$path"
  else
    printf '%s' '-'
  fi
}

validate_input() {
  local label="$1"
  local path="$2"
  local error_file="$3"
  if [[ ! -f "$path" || -L "$path" ]]; then
    printf '%s must be a regular non-symlink file: %s\n' "$label" "$path" >> "$error_file"
    return 1
  fi
  local bytes
  bytes="$(wc -c < "$path" | tr -d '[:space:]')"
  if [[ ! "$bytes" =~ ^[0-9]+$ ]]; then
    printf 'could not determine size for %s\n' "$label" >> "$error_file"
    return 1
  fi
  if (( bytes <= 0 || bytes > 16777216 )); then
    printf '%s must be between 1 and 16777216 bytes\n' "$label" >> "$error_file"
    return 1
  fi
  return 0
}

canonicalize_semantic_model() {
  local input="$1"
  local expected_format="$2"
  local output="$3"
  python3 - "$input" "$expected_format" "$output" <<'PY'
import json
import pathlib
import sys

source, expected_format, destination = sys.argv[1:]
try:
    document = json.loads(pathlib.Path(source).read_text(encoding="utf-8"))
except (OSError, UnicodeError, json.JSONDecodeError) as exc:
    raise SystemExit(f"cannot parse {source}: {exc}")

if not isinstance(document, dict):
    raise SystemExit(f"{source}: top level must be an object")
if document.get("format") != expected_format:
    raise SystemExit(
        f"{source}: format must be {expected_format!r}, got {document.get('format')!r}"
    )
semantic_model = document.get("semanticModel")
if not isinstance(semantic_model, dict):
    raise SystemExit(f"{source}: semanticModel must be an object")

# Objects are key-order independent. Arrays remain ordered because enum order,
# tuple position, precedence, and other list semantics may be significant. Each
# generator is responsible for stable ordering inside unordered collections.
canonical = json.dumps(
    semantic_model,
    ensure_ascii=False,
    allow_nan=False,
    sort_keys=True,
    separators=(",", ":"),
)
pathlib.Path(destination).write_text(canonical + "\n", encoding="utf-8")
PY
}

dpm=""
typespec_sql=""
json_schema_sql=""
typespec_types=""
json_schema_types=""
seaorm_projection=""
diesel_projection=""
shadow=""
output_dir=""

while (( $# > 0 )); do
  case "$1" in
    --dpm) dpm="${2:-}"; shift 2 ;;
    --typespec-sql) typespec_sql="${2:-}"; shift 2 ;;
    --json-schema-sql) json_schema_sql="${2:-}"; shift 2 ;;
    --typespec-types) typespec_types="${2:-}"; shift 2 ;;
    --json-schema-types) json_schema_types="${2:-}"; shift 2 ;;
    --seaorm-projection) seaorm_projection="${2:-}"; shift 2 ;;
    --diesel-projection) diesel_projection="${2:-}"; shift 2 ;;
    --shadow) shadow="${2:-}"; shift 2 ;;
    --output-dir) output_dir="${2:-}"; shift 2 ;;
    *) usage ;;
  esac
done

[[ -n "$dpm" && -n "$typespec_sql" && -n "$json_schema_sql" ]] || usage
[[ -n "$typespec_types" && -n "$json_schema_types" ]] || usage
[[ -n "$seaorm_projection" && -n "$diesel_projection" ]] || usage
[[ -n "$shadow" && -n "$output_dir" ]] || usage
[[ "$shadow" == postgres://* || "$shadow" == postgresql://* ]] || \
  fatal "shadow must use postgres:// or postgresql://"

if [[ -e "$output_dir" || -L "$output_dir" ]]; then
  [[ -d "$output_dir" && ! -L "$output_dir" ]] || \
    fatal "output directory must be a real directory, not a symlink: $output_dir"
else
  mkdir -p -- "$output_dir"
fi
chmod 700 "$output_dir"

sql_plan="$output_dir/typespec-vs-json-schema-plan.json"
typespec_types_canonical="$output_dir/typespec-types.semantic.json"
json_schema_types_canonical="$output_dir/json-schema-types.semantic.json"
types_diff="$output_dir/typespec-vs-json-schema-types.diff"
seaorm_canonical="$output_dir/seaorm-projection.semantic.json"
diesel_canonical="$output_dir/diesel-projection.semantic.json"
orm_diff="$output_dir/seaorm-vs-diesel.diff"
report="$output_dir/peer-authority-certification.json"
sql_errors="$output_dir/typespec-vs-json-schema-sql.errors.txt"
types_errors="$output_dir/typespec-vs-json-schema-types.errors.txt"
orm_errors="$output_dir/seaorm-vs-diesel.errors.txt"

for artifact in \
  "$sql_plan" \
  "$typespec_types_canonical" \
  "$json_schema_types_canonical" \
  "$types_diff" \
  "$seaorm_canonical" \
  "$diesel_canonical" \
  "$orm_diff" \
  "$report" \
  "$sql_errors" \
  "$types_errors" \
  "$orm_errors"; do
  [[ ! -e "$artifact" && ! -L "$artifact" ]] || \
    fatal "refusing to overwrite evidence artifact: $artifact"
done

: > "$sql_errors"
: > "$types_errors"
: > "$orm_errors"
: > "$types_diff"
: > "$orm_diff"

sql_status="pass"
sql_rc=0
sql_inputs_valid="true"
if [[ ! -x "$dpm" || -L "$dpm" ]]; then
  printf 'dpm must be an executable non-symlink file: %s\n' "$dpm" >> "$sql_errors"
  sql_inputs_valid="false"
fi
validate_input "TypeSpec SQL" "$typespec_sql" "$sql_errors" || sql_inputs_valid="false"
validate_input "JSON Schema/OpenAPI SQL" "$json_schema_sql" "$sql_errors" || sql_inputs_valid="false"

if [[ "$sql_inputs_valid" == "false" ]]; then
  sql_status="missing"
  sql_rc=3
  printf '{"error":"required SQL comparison input is missing or invalid"}\n' > "$sql_plan"
else
  set +e
  "$dpm" diff \
    --source-sql "$typespec_sql" \
    --target-sql "$json_schema_sql" \
    --shadow "$shadow" \
    --format json \
    --out "$sql_plan" \
    --fail-on-diff \
    2>> "$sql_errors"
  sql_rc=$?
  set -e
  case "$sql_rc" in
    0) sql_status="pass" ;;
    2|3) sql_status="discrepancy" ;;
    *) sql_status="error" ;;
  esac
  if [[ ! -f "$sql_plan" || -L "$sql_plan" ]]; then
    printf '{"error":"dpm did not produce a SQL comparison plan"}\n' > "$sql_plan"
    printf 'dpm did not produce the SQL comparison plan\n' >> "$sql_errors"
    sql_status="error"
  fi
fi

types_status="pass"
types_inputs_valid="true"
validate_input "TypeSpec type manifest" "$typespec_types" "$types_errors" || types_inputs_valid="false"
validate_input "JSON Schema/OpenAPI type manifest" "$json_schema_types" "$types_errors" || types_inputs_valid="false"

if [[ "$types_inputs_valid" == "false" ]]; then
  types_status="missing"
  printf '{"error":"TypeSpec type manifest unavailable"}\n' > "$typespec_types_canonical"
  printf '{"error":"JSON Schema/OpenAPI type manifest unavailable"}\n' > "$json_schema_types_canonical"
else
  set +e
  canonicalize_semantic_model \
    "$typespec_types" \
    "declmig.generated-types/v1" \
    "$typespec_types_canonical" \
    2>> "$types_errors"
  typespec_types_rc=$?
  canonicalize_semantic_model \
    "$json_schema_types" \
    "declmig.generated-types/v1" \
    "$json_schema_types_canonical" \
    2>> "$types_errors"
  json_schema_types_rc=$?
  set -e
  if (( typespec_types_rc != 0 || json_schema_types_rc != 0 )); then
    types_status="error"
    [[ -f "$typespec_types_canonical" ]] || printf '{"error":"TypeSpec canonicalization failed"}\n' > "$typespec_types_canonical"
    [[ -f "$json_schema_types_canonical" ]] || printf '{"error":"JSON Schema/OpenAPI canonicalization failed"}\n' > "$json_schema_types_canonical"
  elif ! cmp -s -- "$typespec_types_canonical" "$json_schema_types_canonical"; then
    types_status="discrepancy"
    diff -u --label typespec --label json-schema-openapi \
      "$typespec_types_canonical" "$json_schema_types_canonical" \
      > "$types_diff" || true
  fi
fi

orm_status="pass"
orm_inputs_valid="true"
validate_input "SeaORM projection manifest" "$seaorm_projection" "$orm_errors" || orm_inputs_valid="false"
validate_input "Diesel projection manifest" "$diesel_projection" "$orm_errors" || orm_inputs_valid="false"

if [[ "$orm_inputs_valid" == "false" ]]; then
  orm_status="missing"
  printf '{"error":"SeaORM projection manifest unavailable"}\n' > "$seaorm_canonical"
  printf '{"error":"Diesel projection manifest unavailable"}\n' > "$diesel_canonical"
else
  set +e
  canonicalize_semantic_model \
    "$seaorm_projection" \
    "declmig.orm-projection/v1" \
    "$seaorm_canonical" \
    2>> "$orm_errors"
  seaorm_rc=$?
  canonicalize_semantic_model \
    "$diesel_projection" \
    "declmig.orm-projection/v1" \
    "$diesel_canonical" \
    2>> "$orm_errors"
  diesel_rc=$?
  set -e
  if (( seaorm_rc != 0 || diesel_rc != 0 )); then
    orm_status="error"
    [[ -f "$seaorm_canonical" ]] || printf '{"error":"SeaORM canonicalization failed"}\n' > "$seaorm_canonical"
    [[ -f "$diesel_canonical" ]] || printf '{"error":"Diesel canonicalization failed"}\n' > "$diesel_canonical"
  elif ! cmp -s -- "$seaorm_canonical" "$diesel_canonical"; then
    orm_status="discrepancy"
    diff -u --label seaorm --label diesel \
      "$seaorm_canonical" "$diesel_canonical" \
      > "$orm_diff" || true
  fi
fi

python3 - \
  "$report" \
  "$sql_status" \
  "$sql_rc" \
  "$types_status" \
  "$orm_status" \
  "$(sha256_optional "$dpm")" \
  "$(sha256_optional "$typespec_sql")" \
  "$(sha256_optional "$json_schema_sql")" \
  "$(sha256_optional "$typespec_types")" \
  "$(sha256_optional "$json_schema_types")" \
  "$(sha256_optional "$seaorm_projection")" \
  "$(sha256_optional "$diesel_projection")" \
  "$(sha256_file "$sql_plan")" \
  "$(sha256_file "$types_diff")" \
  "$(sha256_file "$orm_diff")" \
  "$sql_errors" \
  "$types_errors" \
  "$orm_errors" <<'PY'
import json
import pathlib
import sys

(
    output,
    sql_status,
    sql_rc,
    types_status,
    orm_status,
    dpm_sha,
    typespec_sql_sha,
    json_schema_sql_sha,
    typespec_types_sha,
    json_schema_types_sha,
    seaorm_sha,
    diesel_sha,
    sql_plan_sha,
    types_diff_sha,
    orm_diff_sha,
    sql_errors_path,
    types_errors_path,
    orm_errors_path,
) = sys.argv[1:]


def optional_sha(value):
    return None if value == "-" else value


def optional_message(path):
    text = pathlib.Path(path).read_text(encoding="utf-8").strip()
    return text or None


statuses = (sql_status, types_status, orm_status)
decision = "continue" if all(status == "pass" for status in statuses) else "pause"
report = {
    "format": "declmig.peer-authority-certification/v1",
    "decision": decision,
    "decisionEligible": decision == "continue",
    "policy": {
        "automaticWinner": False,
        "onDiscrepancy": "pause-and-evaluate",
        "requiredComparisons": [
            "typespec-vs-json-schema-openapi-sql-catalog",
            "typespec-vs-json-schema-openapi-generated-types",
            "seaorm-vs-diesel-orm-projection",
        ],
    },
    "comparisons": [
        {
            "kind": "sql-catalog",
            "left": "typespec",
            "right": "json-schema-openapi",
            "status": sql_status,
            "toolExitCode": int(sql_rc),
            "message": optional_message(sql_errors_path),
            "evidenceSha256": sql_plan_sha,
        },
        {
            "kind": "generated-types",
            "left": "typespec",
            "right": "json-schema-openapi",
            "status": types_status,
            "message": optional_message(types_errors_path),
            "evidenceSha256": types_diff_sha,
        },
        {
            "kind": "orm-projection",
            "left": "seaorm",
            "right": "diesel",
            "status": orm_status,
            "message": optional_message(orm_errors_path),
            "evidenceSha256": orm_diff_sha,
        },
    ],
    "inputs": {
        "dpmSha256": optional_sha(dpm_sha),
        "typespecSqlSha256": optional_sha(typespec_sql_sha),
        "jsonSchemaOpenApiSqlSha256": optional_sha(json_schema_sql_sha),
        "typespecTypesSha256": optional_sha(typespec_types_sha),
        "jsonSchemaOpenApiTypesSha256": optional_sha(json_schema_types_sha),
        "seaOrmProjectionSha256": optional_sha(seaorm_sha),
        "dieselProjectionSha256": optional_sha(diesel_sha),
    },
}
pathlib.Path(output).write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
PY

chmod 600 \
  "$sql_plan" \
  "$typespec_types_canonical" \
  "$json_schema_types_canonical" \
  "$types_diff" \
  "$seaorm_canonical" \
  "$diesel_canonical" \
  "$orm_diff" \
  "$sql_errors" \
  "$types_errors" \
  "$orm_errors" \
  "$report"

if [[ "$sql_status" == "pass" && "$types_status" == "pass" && "$orm_status" == "pass" ]]; then
  printf 'peer authorities agree; decision=continue report=%s\n' "$report"
  exit 0
fi

printf 'peer-authority gate stopped; decision=pause report=%s\n' "$report" >&2
exit 3
