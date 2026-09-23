#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
publisher="$root/scripts/publish-wiki.sh"
checker="$root/scripts/check-wiki.py"
workflow="$root/.github/workflows/publish-wiki.yml"
guidance="$root/CLAUDE.md"

fail() {
  printf 'wiki publish test: %s\n' "$1" >&2
  exit 1
}

[[ -x "$publisher" ]] || fail "publisher is missing or not executable"
[[ -x "$checker" ]] || fail "checker is missing or not executable"
[[ -f "$workflow" ]] || fail "workflow is missing"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
source_dir="$tmp/source"
wiki_repo="$tmp/wiki"
mkdir -p "$source_dir"
git init -q -b master "$wiki_repo"
git -C "$wiki_repo" config user.name test
git -C "$wiki_repo" config user.email test@example.invalid
printf '# stale\n' > "$wiki_repo/Stale.md"
printf '# old\n' > "$wiki_repo/Keep.md"
git -C "$wiki_repo" add --all
git -C "$wiki_repo" commit -q -m seed

printf '# home\n' > "$source_dir/Home.md"
printf '# 首页\n' > "$source_dir/Home-zh-CN.md"
printf '# current\n' > "$source_dir/Keep.md"
printf 'not a wiki page\n' > "$source_dir/ignored.txt"

first_output="$tmp/first-output"
GITHUB_OUTPUT="$first_output" "$publisher" "$source_dir" "$wiki_repo" 0123456789abcdef
[[ "$(<"$first_output")" == "changed=true" ]] || fail "changed publish did not request a push"
[[ -d "$wiki_repo/.git" ]] || fail "publisher removed Git metadata"
[[ "$(git -C "$wiki_repo" branch --show-current)" == "master" ]] || fail "publisher changed branch"
[[ -f "$wiki_repo/Home.md" ]] || fail "publisher omitted a new page"
[[ -f "$wiki_repo/Home-zh-CN.md" ]] || fail "publisher omitted the Chinese page"
[[ "$(<"$wiki_repo/Keep.md")" == "# current" ]] || fail "publisher did not update a page"
[[ ! -e "$wiki_repo/Stale.md" ]] || fail "publisher retained a deleted page"
[[ ! -e "$wiki_repo/ignored.txt" ]] || fail "publisher copied a non-Markdown file"
[[ "$(git -C "$wiki_repo" rev-list --count HEAD)" == "2" ]] || fail "first publish did not create one commit"
[[ "$(git -C "$wiki_repo" log -1 --pretty=%s)" == "Publish wiki from 0123456" ]] || fail "commit does not identify source SHA"

no_change_output="$tmp/no-change-output"
GITHUB_OUTPUT="$no_change_output" "$publisher" "$source_dir" "$wiki_repo" 0123456789abcdef
[[ "$(<"$no_change_output")" == "changed=false" ]] || fail "unchanged publish requested a push"
[[ "$(git -C "$wiki_repo" rev-list --count HEAD)" == "2" ]] || fail "unchanged publish created a commit"

rm "$source_dir/Keep.md"
printf '# renamed\n' > "$source_dir/Renamed.md"
"$publisher" "$source_dir" "$wiki_repo" fedcba9876543210
[[ ! -e "$wiki_repo/Keep.md" ]] || fail "rename retained the old page"
[[ -f "$wiki_repo/Renamed.md" ]] || fail "rename omitted the new page"
[[ "$(git -C "$wiki_repo" rev-list --count HEAD)" == "3" ]] || fail "rename did not create one commit"

for required in \
  'branches: [main]' \
  'workflow_dispatch:' \
  'group: publish-wiki' \
  'cancel-in-progress: false' \
  "if: github.ref == 'refs/heads/main'" \
  'contents: write' \
  'secrets.GITHUB_TOKEN' \
  "x-access-token:\${GH_TOKEN}" \
  'scripts/publish-wiki.sh' \
  "if: steps.mirror.outputs.changed == 'true'" \
  'HEAD:master'; do
  grep -Fq -- "$required" "$workflow" || fail "workflow is missing: $required"
done

if grep -Eq '^[[:space:]]*paths(-ignore)?:' "$workflow"; then
  fail "workflow filters main pushes instead of publishing after every merge"
fi
if grep -Eq 'WIKI_PUBLISHER_(APP_ID|PRIVATE_KEY)|create-github-app-token' "$workflow"; then
  fail "workflow introduces an unnecessary long-lived wiki credential"
fi

grep -Fq 'only source of truth' "$guidance" || fail "guidance no longer makes wiki/ canonical"
grep -Fq 'Never edit the GitHub wiki directly' "$guidance" || fail "guidance permits divergent browser edits"
grep -Fq 'overwritten on the next publish' "$guidance" || fail "guidance omits one-way mirror behavior"
grep -Fq 'Load only the English files for routine agent context' "$guidance" || fail "guidance must require English-only context"
grep -Fq '<Page>-zh-CN.md' "$guidance" || fail "guidance must name separate Chinese files"
if grep -Eq 'Every page is bilingual: an|both language halves|both halves' "$guidance"; then
  fail "guidance still requires both languages in one file"
fi
python3 - "$guidance" <<'PY'
from pathlib import Path
import sys

guidance = Path(sys.argv[1]).read_text(encoding="utf-8")
read_first = guidance.split("## Read first", 1)[1].split("**Canonical", 1)[0]
assert "-zh-CN.md" not in read_first, "routine context must reference English pages only"
PY

# Exercise the checker in throwaway Git repositories so every mutation is a
# tracked wiki source change and cannot affect the real documentation tree.
checker_fixture="$tmp/checker-fixture"
mkdir -p "$checker_fixture/scripts" "$checker_fixture/wiki"
cp "$checker" "$checker_fixture/scripts/check-wiki.py"
cp "$root/Cargo.toml" "$root/Cargo.lock" "$checker_fixture/"
while IFS= read -r manifest; do
  fixture_manifest="$checker_fixture/${manifest#"$root/"}"
  mkdir -p "$(dirname "$fixture_manifest")/src"
  cp "$manifest" "$fixture_manifest"
  : > "$(dirname "$fixture_manifest")/src/lib.rs"
done < <(printf '%s\n' "$root"/crates/*/Cargo.toml)
cp "$root/wiki/"*.md "$checker_fixture/wiki/"
git -C "$checker_fixture" init -q -b main
git -C "$checker_fixture" add Cargo.toml Cargo.lock crates wiki
git -C "$checker_fixture" config user.name test
git -C "$checker_fixture" config user.email test@example.invalid
git -C "$checker_fixture" commit -q -m fixture

assert_checker_rejects() {
  local name="$1"
  local expected="$2"
  local fixture="$tmp/$name"
  shift 2

  cp -R "$checker_fixture" "$fixture"
  "$@" "$fixture"
  git -C "$fixture" add wiki
  if (cd "$fixture" && python3 scripts/check-wiki.py >output 2>&1); then
    fail "checker accepted $name mutation"
  fi
  grep -Fq -- "$expected" "$fixture/output" || {
    printf 'wiki publish test: checker output for %s:\n' "$name" >&2
    cat "$fixture/output" >&2
    fail "checker rejected $name without the expected diagnostic: $expected"
  }
}

replace_fixture_text() {
  local fixture="$1" page="$2" old="$3" new="$4"
  python3 - "$fixture/wiki/$page.md" "$old" "$new" <<'PY'
from pathlib import Path
import sys

path = Path(sys.argv[1])
text = path.read_text(encoding="utf-8")
assert sys.argv[2] in text, (path, sys.argv[2])
path.write_text(text.replace(sys.argv[2], sys.argv[3]), encoding="utf-8")
PY
}

mutate_mixed_language() {
  # Preserve heading parity so only the legacy-marker rule can reject the mutation.
  printf '\n## English\n\n## 中文\n' >> "$1/wiki/Usage.md"
  printf '\n## English\n\n## 中文\n' >> "$1/wiki/Usage-zh-CN.md"
}

mutate_missing_chinese_page() {
  rm "$1/wiki/Usage-zh-CN.md"
}

mutate_missing_english_page() {
  rm "$1/wiki/Usage.md"
}

mutate_heading_mismatch() {
  replace_fixture_text "$1" Usage '### Install and first launch' '#### Install and first launch'
}

mutate_nested_page() {
  mkdir -p "$1/wiki/nested"
  printf '# Nested\n' > "$1/wiki/nested/Page.md"
}

mutate_non_ascii_page() {
  printf '# malformed\n' > "$1/wiki/坏页.md"
}

mutate_missing_switch() {
  replace_fixture_text "$1" Usage '[简体中文](Usage-zh-CN)' '简体中文'
}

mutate_missing_chinese_switch() {
  replace_fixture_text "$1" Usage-zh-CN '[English](Usage)' 'English'
}

mutate_cross_language_link() {
  replace_fixture_text "$1" Usage-zh-CN '](Keybindings-zh-CN)' '](Keybindings)'
}

mutate_english_cross_language_link() {
  replace_fixture_text "$1" Usage '](Keybindings)' '](Keybindings-zh-CN)'
}

mutate_md_link() {
  replace_fixture_text "$1" Usage '](Keybindings)' '](Keybindings.md)'
}

mutate_md_anchor_link() {
  replace_fixture_text "$1" Usage '](Keybindings)' '](Keybindings.md#bindings)'
}

mutate_unknown_link() {
  replace_fixture_text "$1" Usage '](Keybindings)' '](Missing-Page)'
}

mutate_independent_file_fence_state() {
  # An unclosed fence in one file must not hide broken links in its translation.
  printf '\n```text\nunclosed English fence\n' >> "$1/wiki/Usage.md"
  replace_fixture_text "$1" Usage-zh-CN '](Keybindings-zh-CN)' '](Missing-Page-zh-CN)'
}

mutate_external_home_english_link() {
  replace_fixture_text "$1" Home '](Configuration)' '](mailto:Configuration)'
}

mutate_missing_home_english_link() {
  replace_fixture_text "$1" Home '](Configuration)' '](Usage)'
}

mutate_missing_home_chinese_link() {
  replace_fixture_text "$1" Home-zh-CN '](Configuration-zh-CN)' '](Usage-zh-CN)'
}

mutate_missing_english_crate() {
  replace_fixture_text "$1" Crate-Reference sonicterm-types missing-english-crate
}

mutate_missing_chinese_crate() {
  replace_fixture_text "$1" Crate-Reference-zh-CN sonicterm-types missing-chinese-crate
}

# Separate files are the context boundary; examples may still contain Unicode text.
[[ -f "$checker_fixture/wiki/Usage-zh-CN.md" ]] || fail "Chinese documentation must have a separate file"
(
  cd "$checker_fixture"
  python3 scripts/check-wiki.py
) || fail "checker rejected the repository wiki"

assert_checker_rejects mixed-language 'wiki/Usage.md: legacy language marker' mutate_mixed_language
assert_checker_rejects mixed-language-chinese 'wiki/Usage-zh-CN.md: legacy language marker' mutate_mixed_language
assert_checker_rejects missing-chinese-page 'wiki/Usage.md: missing language counterpart: Usage-zh-CN' mutate_missing_chinese_page
assert_checker_rejects missing-english-page 'wiki/Usage-zh-CN.md: missing language counterpart: Usage' mutate_missing_english_page
assert_checker_rejects heading-mismatch 'heading-depth sequences differ' mutate_heading_mismatch
assert_checker_rejects nested-page 'wiki/nested/Page.md:' mutate_nested_page
assert_checker_rejects non-ascii-page 'wiki/坏页.md:' mutate_non_ascii_page
assert_checker_rejects missing-switch 'missing language-switch link' mutate_missing_switch
assert_checker_rejects missing-chinese-switch 'missing language-switch link' mutate_missing_chinese_switch
assert_checker_rejects cross-language-link 'cross-page link must stay in the same language' mutate_cross_language_link
assert_checker_rejects english-cross-language-link 'cross-page link must stay in the same language' mutate_english_cross_language_link
assert_checker_rejects md-link 'wiki/Usage.md:' mutate_md_link
assert_checker_rejects md-anchor-link 'cross-page link must omit .md' mutate_md_anchor_link
assert_checker_rejects unknown-link 'wiki/Usage.md:' mutate_unknown_link
assert_checker_rejects independent-file-fence-state 'cross-page link target does not exist: Missing-Page-zh-CN' mutate_independent_file_fence_state
assert_checker_rejects external-home-english-link 'wiki/Home.md:' mutate_external_home_english_link
assert_checker_rejects missing-home-english-link 'wiki/Home.md:' mutate_missing_home_english_link
assert_checker_rejects missing-home-chinese-link 'wiki/Home-zh-CN.md:' mutate_missing_home_chinese_link
assert_checker_rejects missing-english-crate 'wiki/Crate-Reference.md:' mutate_missing_english_crate
assert_checker_rejects missing-chinese-crate 'wiki/Crate-Reference-zh-CN.md:' mutate_missing_chinese_crate

printf 'wiki publish test: ok\n'
