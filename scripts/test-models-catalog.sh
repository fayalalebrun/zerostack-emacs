#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
mkdir -p "$root/target"
tmp="$(mktemp -d "$root/target/catalog-test.XXXXXX")"
mkdir -p "$tmp/scripts" "$tmp/data" "$tmp/bin"
cp "$root/scripts/gen-models-catalog.sh" "$tmp/scripts/"
cat > "$tmp/bin/curl" <<'SH'
#!/usr/bin/env bash
cat "$CATALOG_TEST_API"
SH
chmod +x "$tmp/bin/curl"
export PATH="$tmp/bin:$PATH"
export CATALOG_TEST_API="$tmp/api.json"
cat > "$tmp/data/models.json" <<'JSON'
{"opencode-go":[{"id":"known","name":"Known","context":128000},{"id":"legacy","name":"Legacy","context":256000}],"local-provider":[{"id":"keep","name":"Keep","context":64000}]}
JSON
cat > "$CATALOG_TEST_API" <<'JSON'
{"opencode-go":{"models":{"known":{"limit":{"context":1050000,"input":922000}}}}}
JSON
bash "$tmp/scripts/gen-models-catalog.sh"
jq -e '.["opencode-go"] == [{"id":"known","name":"Known","context":1050000,"input":922000},{"id":"legacy","name":"Legacy","context":256000}] and .["local-provider"][0].context == 64000' "$tmp/data/models.json"
printf '%s\n' '{"opencode-go":{"models":{"known":{"limit":{"context":1000000}}}}}' > "$CATALOG_TEST_API"
bash "$tmp/scripts/gen-models-catalog.sh"
jq -e '.["opencode-go"][0].context == 1000000 and (.["opencode-go"][0] | has("input") | not)' "$tmp/data/models.json"
cp "$tmp/data/models.json" "$tmp/expected.json"
printf '%s\n' 'not JSON' > "$CATALOG_TEST_API"
if bash "$tmp/scripts/gen-models-catalog.sh"; then
    exit 1
fi
cmp "$tmp/expected.json" "$tmp/data/models.json"
printf '%s\n' 'Catalog generation regression tests passed'
