#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
subject="$script_dir/certify-peer-authorities.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

cat > "$tmp/fake-dpm" <<'DPM'
#!/usr/bin/env bash
set -euo pipefail
source_sql=""
target_sql=""
out=""
while (( $# > 0 )); do
  case "$1" in
    diff|--format|json|--shadow|--fail-on-diff) shift ;;
    --source-sql) source_sql="$2"; shift 2 ;;
    --target-sql) target_sql="$2"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    postgres://*|postgresql://*) shift ;;
    *) shift ;;
  esac
done
[[ -n "$source_sql" && -n "$target_sql" && -n "$out" ]]
if cmp -s "$source_sql" "$target_sql"; then
  printf '{"summary":{"total":0}}\n' > "$out"
  exit 0
fi
printf '{"summary":{"total":1}}\n' > "$out"
exit 2
DPM
chmod 700 "$tmp/fake-dpm"

printf 'CREATE TABLE example(id bigint PRIMARY KEY);\n' > "$tmp/typespec.sql"
cp "$tmp/typespec.sql" "$tmp/json-schema.sql"

cat > "$tmp/typespec-types.json" <<'JSON'
{
  "format": "declmig.generated-types/v1",
  "authority": "typespec",
  "generator": {"name": "typespec-test", "version": "1"},
  "semanticModel": {
    "models": [{"name": "Example", "properties": [{"name": "id", "type": "int64", "required": true}]}]
  }
}
JSON
cat > "$tmp/json-schema-types.json" <<'JSON'
{
  "format": "declmig.generated-types/v1",
  "authority": "json-schema-openapi",
  "generator": {"name": "json-schema-test", "version": "2"},
  "semanticModel": {
    "models": [{"name": "Example", "properties": [{"name": "id", "type": "int64", "required": true}]}]
  }
}
JSON
cat > "$tmp/seaorm.json" <<'JSON'
{
  "format": "declmig.orm-projection/v1",
  "orm": "seaorm",
  "semanticModel": {
    "tables": [{"name": "example", "columns": [{"name": "id", "sqlType": "int8", "nullable": false, "primaryKey": true}]}]
  }
}
JSON
cat > "$tmp/diesel.json" <<'JSON'
{
  "format": "declmig.orm-projection/v1",
  "orm": "diesel",
  "semanticModel": {
    "tables": [{"name": "example", "columns": [{"name": "id", "sqlType": "int8", "nullable": false, "primaryKey": true}]}]
  }
}
JSON

bash -n "$subject"
bash "$subject" \
  --dpm "$tmp/fake-dpm" \
  --typespec-sql "$tmp/typespec.sql" \
  --json-schema-sql "$tmp/json-schema.sql" \
  --typespec-types "$tmp/typespec-types.json" \
  --json-schema-types "$tmp/json-schema-types.json" \
  --seaorm-projection "$tmp/seaorm.json" \
  --diesel-projection "$tmp/diesel.json" \
  --shadow postgres://localhost/shadow \
  --output-dir "$tmp/pass"
python3 - "$tmp/pass/peer-authority-certification.json" <<'PY'
import json
import pathlib
import sys
report = json.loads(pathlib.Path(sys.argv[1]).read_text())
assert report["decision"] == "continue"
assert report["decisionEligible"] is True
assert all(item["status"] == "pass" for item in report["comparisons"])
PY

python3 - "$tmp/json-schema-types.json" <<'PY'
import json
import pathlib
import sys
path = pathlib.Path(sys.argv[1])
document = json.loads(path.read_text())
document["semanticModel"]["models"][0]["properties"][0]["type"] = "string"
path.write_text(json.dumps(document, indent=2) + "\n")
PY

set +e
bash "$subject" \
  --dpm "$tmp/fake-dpm" \
  --typespec-sql "$tmp/typespec.sql" \
  --json-schema-sql "$tmp/json-schema.sql" \
  --typespec-types "$tmp/typespec-types.json" \
  --json-schema-types "$tmp/json-schema-types.json" \
  --seaorm-projection "$tmp/seaorm.json" \
  --diesel-projection "$tmp/diesel.json" \
  --shadow postgres://localhost/shadow \
  --output-dir "$tmp/pause"
rc=$?
set -e
[[ "$rc" -eq 3 ]]
python3 - "$tmp/pause/peer-authority-certification.json" <<'PY'
import json
import pathlib
import sys
report = json.loads(pathlib.Path(sys.argv[1]).read_text())
assert report["decision"] == "pause"
assert report["decisionEligible"] is False
assert any(item["status"] == "discrepancy" for item in report["comparisons"])
PY

grep -q '^--- typespec' "$tmp/pause/typespec-vs-json-schema-types.diff"
printf 'peer-authority certification tests passed\n'

set +e
bash "$subject" \
  --dpm "$tmp/fake-dpm" \
  --typespec-sql "$tmp/typespec.sql" \
  --json-schema-sql "$tmp/does-not-exist.sql" \
  --typespec-types "$tmp/typespec-types.json" \
  --json-schema-types "$tmp/json-schema-types.json" \
  --seaorm-projection "$tmp/seaorm.json" \
  --diesel-projection "$tmp/diesel.json" \
  --shadow postgres://localhost/shadow \
  --output-dir "$tmp/missing"
rc=$?
set -e
[[ "$rc" -eq 3 ]]
python3 - "$tmp/missing/peer-authority-certification.json" <<'PY'
import json
import pathlib
import sys
report = json.loads(pathlib.Path(sys.argv[1]).read_text())
assert report["decision"] == "pause"
assert report["decisionEligible"] is False
assert report["comparisons"][0]["status"] == "missing"
assert report["policy"]["automaticWinner"] is False
PY

printf 'peer-authority missing-artifact test passed\n'
