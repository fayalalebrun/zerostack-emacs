#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
mkdir -p "$root/target"
tmp="$(mktemp -d "$root/target/catalog-test.XXXXXX")"
mkdir -p "$tmp/scripts" "$tmp/data" "$tmp/bin"
cp "$root/scripts/gen-models-catalog.sh" "$tmp/scripts/"
cat > "$tmp/bin/curl" <<'SH'
#!/usr/bin/env bash
case "$*" in
  *models.dev*) cat "$CATALOG_TEST_API" ;;
  *opencode.ai*) cat "$CATALOG_TEST_GO" ;;
  *) exit 2 ;;
esac
SH
chmod +x "$tmp/bin/curl"
export PATH="$tmp/bin:$PATH"
export CATALOG_TEST_API="$tmp/api.json"
export CATALOG_TEST_GO="$tmp/go.json"
cat > "$tmp/data/models.json" <<'JSON'
{"opencode-go":[{"id":"known","name":"Known","context":128000},{"id":"legacy","name":"Legacy","context":256000}],"local-provider":[{"id":"keep","name":"Keep","context":64000}]}
JSON
cat > "$CATALOG_TEST_API" <<'JSON'
{"opencode-go":{"models":{"known":{"name":"Known Current","limit":{"context":1050000,"input":922000}},"new":{"name":"New","limit":{"context":500000}},"deepseek-v4-flash":{"name":"DeepSeek V4 Flash","limit":{"context":1000000}}}}}
JSON
cat > "$CATALOG_TEST_GO" <<'JSON'
{"data":[{"id":"new"},{"id":"deepseek-flash"},{"id":"known"}]}
JSON
bash "$tmp/scripts/gen-models-catalog.sh"
jq -e '.["opencode-go"] == [{"id":"known","name":"Known","context":1050000,"input":922000},{"id":"new","name":"New","context":500000},{"id":"deepseek-flash","name":"DeepSeek Flash","context":1000000}] and .["local-provider"][0].context == 64000' "$tmp/data/models.json"
printf '%s\n' '{"opencode-go":{"models":{"known":{"name":"Known Current","limit":{"context":1000000}},"new":{"name":"New","limit":{"context":500000}},"deepseek-v4-flash":{"name":"DeepSeek V4 Flash","limit":{"context":1000000}}}}}' > "$CATALOG_TEST_API"
bash "$tmp/scripts/gen-models-catalog.sh"
jq -e '(."opencode-go"[] | select(.id == "known")) as $known | $known.context == 1000000 and ($known | has("input") | not)' "$tmp/data/models.json"
cp "$tmp/data/models.json" "$tmp/expected.json"
printf '%s\n' 'not JSON' > "$CATALOG_TEST_API"
if bash "$tmp/scripts/gen-models-catalog.sh"; then
    exit 1
fi
cmp "$tmp/expected.json" "$tmp/data/models.json"
printf '%s\n' '{"opencode-go":{"models":{}}}' > "$CATALOG_TEST_API"
printf '%s\n' '{"data":[]}' > "$CATALOG_TEST_GO"
if bash "$tmp/scripts/gen-models-catalog.sh"; then
    exit 1
fi
cmp "$tmp/expected.json" "$tmp/data/models.json"
printf '%s\n' 'Catalog generation regression tests passed'
