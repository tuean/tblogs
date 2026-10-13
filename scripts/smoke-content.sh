#!/usr/bin/env bash
#
# End-to-end test for the file-backed content directory (TBLOGS_CONTENT_DIR).
#
# Builds a Typora-style fixture directory, waits for the server to import it,
# then checks importing, asset URL rewriting, media serving, the read-only
# admin rules, live edits and deletions.
#
# Start an instance pointed at the fixture first, with a short sync interval.
# The fixture directory has to exist before the server starts: a missing
# TBLOGS_CONTENT_DIR is a hard startup error, on purpose, so a typo is caught
# rather than silently serving an empty blog.
#
#   mkdir -p /tmp/tblogs-smoke-content
#   rm -f data/smoke.db
#   TBLOGS_DATABASE_URL=sqlite://data/smoke.db \
#   TBLOGS_CONTENT_DIR=/tmp/tblogs-smoke-content \
#   TBLOGS_SYNC_INTERVAL=2 \
#   cargo run &
#
#   ./scripts/smoke-content.sh
#
# Environment:
#   BASE          server root           (default http://127.0.0.1:8080)
#   CONTENT_DIR   fixture directory     (default /tmp/tblogs-smoke-content)
#   TBLOGS_DB     sqlite file, enables the read-only checks
#   SYNC_WAIT     seconds to wait for a sync (default 20)

set -uo pipefail

BASE="${BASE:-http://127.0.0.1:8080}"
CONTENT_DIR="${CONTENT_DIR:-/tmp/tblogs-smoke-content}"
TBLOGS_DB="${TBLOGS_DB:-}"
SYNC_WAIT="${SYNC_WAIT:-20}"

PASS=0
FAIL=0

ok()  { printf '  \033[32mok\033[0m   %s\n' "$1"; PASS=$((PASS + 1)); }
bad() { printf '  \033[31mFAIL\033[0m %s\n' "$1"; FAIL=$((FAIL + 1)); }

check() {
  if [[ "$2" == "$3" ]]; then ok "$1"; else bad "$1 (expected '$2', got '$3')"; fi
}

check_contains() {
  if [[ "$3" == *"$2"* ]]; then ok "$1"; else bad "$1 (missing '$2')"; fi
}

check_missing() {
  if [[ "$3" != *"$2"* ]]; then ok "$1"; else bad "$1 (unexpectedly found '$2')"; fi
}

status() { curl -s -o /dev/null -w '%{http_code}' "$@"; }
body()   { curl -s "$@"; }

# Poll until a URL returns 200, so the test does not depend on sync timing.
wait_for() {
  local url="$1" deadline=$((SECONDS + SYNC_WAIT))
  while (( SECONDS < deadline )); do
    [[ "$(status "$url")" == "200" ]] && return 0
    sleep 1
  done
  return 1
}

wait_gone() {
  local url="$1" deadline=$((SECONDS + SYNC_WAIT))
  while (( SECONDS < deadline )); do
    [[ "$(status "$url")" == "404" ]] && return 0
    sleep 1
  done
  return 1
}

echo "content-directory smoke test"
echo "  server:  $BASE"
echo "  fixture: $CONTENT_DIR"
echo

# Fail early and clearly when nothing is listening, rather than reporting every
# individual check as a failure.
if [[ "$(status --max-time 5 "$BASE/healthz")" != "200" ]]; then
  echo "error: no server is answering at $BASE" >&2
  echo "       start one pointed at $CONTENT_DIR (see the header of this file)" >&2
  exit 2
fi

# ---------------------------------------------------------------- fixture
rm -rf "$CONTENT_DIR"
mkdir -p "$CONTENT_DIR/assets" "$CONTENT_DIR/notes/nested.assets"

# A genuinely valid 1x1 PNG, so byte-for-byte serving can be verified.
PNG_B64='iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFAAH/q842iQAAAABJRU5ErkJggg=='
png() { printf '%s' "$PNG_B64" | base64 -d > "$1"; }

png "$CONTENT_DIR/assets/pic.png"
png "$CONTENT_DIR/assets/我的图.png"
png "$CONTENT_DIR/notes/nested.assets/diagram.png"

cat > "$CONTENT_DIR/2026-01-05-hello.md" <<'EOF'
---
tags: [Typora, 图片]
summary: 一篇带图片的文章
---

# 你好，内容目录

相对路径图片：

![本地图片](assets/pic.png)

Typora 调整大小后会生成原始 HTML：

<img src="assets/pic.png" alt="resized" style="zoom:50%;" />

中文文件名：

![中文图](assets/我的图.png)

远程图片应保持原样：

![remote](https://example.com/x.png)
EOF

cat > "$CONTENT_DIR/notes/nested.md" <<'EOF'
---
title: 嵌套目录
tags: [嵌套]
---

图片在子目录里：

![diagram](nested.assets/diagram.png)
EOF

cat > "$CONTENT_DIR/draft.md" <<'EOF'
---
title: 草稿
draft: true
---

不应出现在公开页面。
EOF

# A Markdown file inside the image folder: it becomes a post, but must never be
# downloadable through /media.
cat > "$CONTENT_DIR/assets/source.md" <<'EOF'
# 源文件

这个文件不应该能通过 /media 下载。
EOF

echo "fixture written: $(find "$CONTENT_DIR" -type f | wc -l | tr -d ' ') files"
echo

# ---------------------------------------------------------------- import
echo "importing"
if wait_for "$BASE/posts/hello"; then
  ok "a Markdown file became a reachable article (/posts/hello)"
else
  bad "article never appeared; is TBLOGS_CONTENT_DIR pointed at $CONTENT_DIR?"
fi

if wait_for "$BASE/posts/nested"; then
  ok "a file in a subdirectory was imported"
else
  bad "nested article never appeared"
fi

home=$(body "$BASE/")
check_contains "the file's # heading became the title" "你好，内容目录" "$home"
check_contains "tags from front matter were applied" "图片" "$home"

# ---------------------------------------------------------------- draft
echo
echo "draft handling"
check "a draft is not publicly reachable" "404" "$(status "$BASE/posts/draft")"
wait_for "$BASE/admin" >/dev/null
check_contains "a draft is still visible in the admin list" "草稿" "$(body "$BASE/admin")"

# ---------------------------------------------------------------- assets
echo
echo "asset serving"
check "PNG is served" "200" "$(status "$BASE/media/assets/pic.png")"
check "PNG keeps its content type" "image/png" \
  "$(curl -s -o /dev/null -w '%{content_type}' "$BASE/media/assets/pic.png")"

curl -s "$BASE/media/assets/pic.png" -o /tmp/tblogs-smoke-served.png
if cmp -s /tmp/tblogs-smoke-served.png "$CONTENT_DIR/assets/pic.png"; then
  ok "served bytes match the file on disk"
else
  bad "served bytes differ from the file on disk"
fi

check "a CJK file name is served" "200" \
  "$(status "$BASE/media/assets/%E6%88%91%E7%9A%84%E5%9B%BE.png")"
check "a nested asset is served" "200" "$(status "$BASE/media/notes/nested.assets/diagram.png")"

# Revalidation: a browser must be able to avoid re-downloading.
etag=$(curl -s -D- -o /dev/null "$BASE/media/assets/pic.png" | tr -d '\r' \
  | sed -n 's/^[Ee][Tt][Aa][Gg]: //p')
if [[ -n "$etag" ]]; then
  check "ETag revalidation returns 304" "304" \
    "$(status -H "If-None-Match: $etag" "$BASE/media/assets/pic.png")"
else
  bad "no ETag was returned"
fi

# ---------------------------------------------------------------- privacy
echo
echo "the content directory is not browsable"
check "a Markdown source is not downloadable" "404" "$(status "$BASE/media/assets/source.md")"
check "path traversal upward is refused" "404" \
  "$(status --path-as-is "$BASE/media/../Cargo.toml")"
check "path traversal out of the root is refused" "404" \
  "$(status --path-as-is "$BASE/media/assets/../../etc/hosts")"
check "a dotfile is refused" "404" "$(status "$BASE/media/.hidden")"

# ---------------------------------------------------------------- rewriting
echo
echo "asset URL rewriting"
article=$(body "$BASE/posts/hello")
check_contains "a relative image was repointed at /media" 'src="/media/assets/pic.png"' "$article"
check_contains "an image in raw HTML was repointed too" 'src="/media/assets/pic.png"' "$article"
check_contains "a CJK image name was percent-encoded" \
  'src="/media/assets/%E6%88%91%E7%9A%84%E5%9B%BE.png"' "$article"
check_contains "a remote image was left alone" 'src="https://example.com/x.png"' "$article"
check_contains "the rest of a rewritten tag survived" 'zoom:50%' "$article"

nested_article=$(body "$BASE/posts/nested")
check_contains "a nested document resolves assets against its own directory" \
  'src="/media/notes/nested.assets/diagram.png"' "$nested_article"

# ---------------------------------------------------------------- read-only
echo
echo "read-only in the web admin"
if [[ -n "$TBLOGS_DB" ]] && command -v sqlite3 >/dev/null 2>&1 && [[ -f "$TBLOGS_DB" ]]; then
  file_id=$(sqlite3 "$TBLOGS_DB" \
    "SELECT id FROM posts WHERE source_path IS NOT NULL LIMIT 1;" 2>/dev/null || true)

  if [[ -n "$file_id" ]]; then
    check "editing a file-backed post is refused" "409" "$(status "$BASE/admin/posts/$file_id/edit")"
    check "deleting a file-backed post is refused" "409" \
      "$(status -X POST "$BASE/admin/posts/$file_id/delete")"
    check "changing its state is refused" "409" \
      "$(status -X POST "$BASE/admin/posts/$file_id/publish")"
  else
    bad "could not find a file-backed post id"
  fi
else
  echo "  (skipped: set TBLOGS_DB to the sqlite file to check the read-only rules)"
fi

admin=$(body "$BASE/admin")
check_contains "the admin list names the source file" "2026-01-05-hello.md" "$admin"

# ---------------------------------------------------------------- live edits
echo
echo "live edits"
# The watcher must pick up a change without a restart.
cat > "$CONTENT_DIR/notes/nested.md" <<'EOF'
---
title: 改过的标题
tags: [嵌套, 新标签]
---

正文已经被改写。

![diagram](nested.assets/diagram.png)
EOF

if wait_for "$BASE/posts/nested"; then
  for _ in $(seq 1 "$SYNC_WAIT"); do
    body "$BASE/posts/nested" | grep -q '改过的标题' && break
    sleep 1
  done
fi

edited=$(body "$BASE/posts/nested")
check_contains "an edit was picked up without a restart" "改过的标题" "$edited"
check_contains "the rewritten body is live" "正文已经被改写" "$edited"
check_contains "tags were updated" "新标签" "$edited"
check_contains "assets still resolve after the edit" \
  'src="/media/notes/nested.assets/diagram.png"' "$edited"

# ---------------------------------------------------------------- deletions
echo
echo "deletions"
rm -f "$CONTENT_DIR/2026-01-05-hello.md"
if wait_gone "$BASE/posts/hello"; then
  ok "deleting the file removed the article"
else
  bad "the article survived deletion of its file"
fi

check "the removed article is gone from the feed" "404" "$(status "$BASE/posts/hello")"

echo
if [[ "$FAIL" -eq 0 ]]; then
  printf '\033[32m%d checks passed\033[0m\n' "$PASS"
  exit 0
fi

printf '\033[31m%d passed, %d FAILED\033[0m\n' "$PASS" "$FAIL"
exit 1
