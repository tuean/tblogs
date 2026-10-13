#!/usr/bin/env bash
#
# End-to-end test for the split architecture.
#
# Runs the whole story the way an author would:
#
#   fixture directory -> tblogs-admin build -> tblogs-server serves it
#                     -> edit + rebuild -> server reloads by itself
#                     -> tblogs-admin bundle -> check the deployment files
#
# Self-contained: it builds its own fixture in a temporary directory, starts the
# server on a free port, and tears everything down afterwards. Requires a prior
# `cargo build` (release is preferred, debug is used as a fallback).
#
# Environment:
#   BIN_DIR   directory holding the two binaries (default target/release or debug)
#   PORT      port for the throwaway server (default 8099)
#   KEEP      set to 1 to leave the temporary directory in place

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ -z "${BIN_DIR:-}" ]]; then
  if [[ -x "$ROOT/target/release/tblogs-admin" ]]; then
    BIN_DIR="$ROOT/target/release"
  elif [[ -x "$ROOT/target/debug/tblogs-admin" ]]; then
    BIN_DIR="$ROOT/target/debug"
  else
    echo "no binaries found; run 'cargo build --release' first" >&2
    exit 2
  fi
fi

ADMIN="$BIN_DIR/tblogs-admin"
SERVER="$BIN_DIR/tblogs-server"
PORT="${PORT:-8099}"
BASE="http://127.0.0.1:$PORT"

for binary in "$ADMIN" "$SERVER"; do
  if [[ ! -x "$binary" ]]; then
    echo "missing $binary; run 'cargo build --release' first" >&2
    exit 2
  fi
done

WORK="$(mktemp -d)"
CONTENT="$WORK/content"
DIST="$WORK/dist"
SERVER_PID=""
PASS=0
FAIL=0

cleanup() {
  if [[ -n "$SERVER_PID" ]]; then
    kill "$SERVER_PID" 2>/dev/null
    wait "$SERVER_PID" 2>/dev/null
  fi
  if [[ "${KEEP:-0}" == "1" ]]; then
    echo "kept: $WORK"
  else
    rm -rf "$WORK"
  fi
}
trap cleanup EXIT

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

echo "tblogs end-to-end test"
echo "  binaries: $BIN_DIR"
echo "  workdir:  $WORK"
echo

# ---------------------------------------------------------------- fixture
mkdir -p "$CONTENT/assets" "$CONTENT/notes/nested.assets"

# A genuinely valid 1x1 PNG, so byte-for-byte serving can be checked.
PNG_B64='iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFAAH/q842iQAAAABJRU5ErkJggg=='
png() { printf '%s' "$PNG_B64" | base64 -d > "$1"; }

png "$CONTENT/assets/pic.png"
png "$CONTENT/assets/我的图.png"
png "$CONTENT/notes/nested.assets/diagram.png"

cat > "$CONTENT/2026-01-05-hello.md" <<'EOF'
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

cat > "$CONTENT/notes/nested.md" <<'EOF'
---
title: 嵌套目录
tags: [嵌套]
---

图片在子目录里：

![diagram](nested.assets/diagram.png)
EOF

# A CRLF document with a cover, which is what an editor on Windows produces.
# Regression: the body offset used to drift on CRLF and panic on multi-byte text.
printf -- '---\r\ntitle: CRLF 文档\r\nimage: https://cdn.example.com/cover.jpg\r\n---\r\n\r\n# 正文\r\n\r\nWindows 换行。\r\n' > "$CONTENT/crlf.md"

cat > "$CONTENT/draft.md" <<'EOF'
---
title: 草稿
draft: true
---

不应出现在公开页面。
EOF

# A Markdown file inside the image folder: it becomes a post, but must never be
# downloadable through /media.
cat > "$CONTENT/assets/source.md" <<'EOF'
# 源文件

这个文件不应该能通过 /media 下载。
EOF

# ---------------------------------------------------------------- build
echo "build"
# Run from a neutral directory: the client is a portable executable and must
# not depend on a schema directory sitting beside it in the source tree.
cd /
build_output="$("$ADMIN" build \
  --content-dir "$CONTENT" \
  --out "$DIST" \
  --site-url https://blog.test.example \
  --title 内容目录测试 \
  --description '本地目录写作' 2>&1)"
build_status=$?

cd "$ROOT"
check "tblogs-admin build exits 0" "0" "$build_status"
if [[ "$build_status" != "0" ]]; then
  echo "$build_output" >&2
fi

check "package contains site.db" "yes" "$([[ -f "$DIST/site.db" ]] && echo yes || echo no)"
check "package contains media/" "yes" "$([[ -d "$DIST/media" ]] && echo yes || echo no)"
check "Markdown sources are not mirrored" "no" \
  "$([[ -f "$DIST/media/assets/source.md" ]] && echo yes || echo no)"
check "assets are mirrored with their paths" "yes" \
  "$([[ -f "$DIST/media/notes/nested.assets/diagram.png" ]] && echo yes || echo no)"
check "no staging directory is left behind" "no" \
  "$([[ -d "$DIST/.staging" ]] && echo yes || echo no)"

# The published database must open read-only, which is how the server uses it.
if sqlite3 "file:$DIST/site.db?mode=ro" "SELECT count(*) FROM posts;" >/dev/null 2>&1; then
  ok "the published database opens read-only"
else
  bad "the published database does not open read-only"
fi

check "no write-ahead log is left beside the database" "no" \
  "$([[ -f "$DIST/site.db-wal" ]] && echo yes || echo no)"

# ---------------------------------------------------------------- serve
echo
echo "serve"
"$SERVER" --data-dir "$DIST" --bind "127.0.0.1:$PORT" --reload-interval-secs 1 \
  >"$WORK/server.log" 2>&1 &
SERVER_PID=$!

for _ in $(seq 1 40); do
  [[ "$(status --max-time 2 "$BASE/healthz")" == "200" ]] && break
  sleep 0.25
done

if [[ "$(status --max-time 5 "$BASE/healthz")" != "200" ]]; then
  bad "the server did not come up"
  echo "--- server log ---" >&2
  cat "$WORK/server.log" >&2
else
  ok "the server is up"
fi

echo
echo "routes"
check "GET / -> 200" "200" "$(status "$BASE/")"
check "GET /posts/hello -> 200" "200" "$(status "$BASE/posts/hello")"
check "GET /posts/nested -> 200" "200" "$(status "$BASE/posts/nested")"
check "GET /tags -> 200" "200" "$(status "$BASE/tags")"
check "GET /tags/nope -> 404" "404" "$(status "$BASE/tags/nope")"
check "GET /feed.xml -> 200" "200" "$(status "$BASE/feed.xml")"
check "GET /healthz -> 200" "200" "$(status "$BASE/healthz")"
check "GET /static/style.css -> 200" "200" "$(status "$BASE/static/style.css")"
check "GET /posts/missing -> 404" "404" "$(status "$BASE/posts/missing")"

echo
echo "the server is read-only"
check "GET /admin -> 404" "404" "$(status "$BASE/admin")"
check "GET /admin/login -> 404" "404" "$(status "$BASE/admin/login")"
check "POST /admin/new -> 404" "404" \
  "$(status -X POST "$BASE/admin/new" --data-urlencode 'title=x')"
check_missing "no admin link in the navigation" 'href="/admin"' "$(body "$BASE/")"

echo
echo "settings travel with the package"
check_contains "the title comes from the package" "内容目录测试" "$(body "$BASE/")"
feed="$(body "$BASE/feed.xml")"
check_contains "RSS links use the packaged site URL" "<link>https://blog.test.example" "$feed"
check_contains "the feed carries items" "<item>" "$feed"

echo
echo "content"
home="$(body "$BASE/")"
check_contains "a file's # heading became the title" "你好，内容目录" "$home"
check_contains "tags from front matter were applied" "图片" "$home"
check "a draft is not reachable" "404" "$(status "$BASE/posts/draft")"

# A CRLF document must import, and its cover must render.
check "a CRLF document was imported" "200" "$(status "$BASE/posts/crlf")"
check_contains "a CRLF body parsed past the front matter" "Windows 换行" "$(body "$BASE/posts/crlf")"
check_contains "the cover from front matter is rendered" \
  'class="post-hero" src="https://cdn.example.com/cover.jpg"' "$(body "$BASE/posts/crlf")"
check_contains "the cover also appears as an index thumbnail" \
  'class="post-thumb"' "$(body "$BASE/")"
check_missing "front matter does not leak into the body" "title: CRLF" "$(body "$BASE/posts/crlf")"
check_missing "a draft is not in the index" "不应出现在公开页面" "$home"

echo
echo "assets"
check "PNG is served" "200" "$(status "$BASE/media/assets/pic.png")"
check "PNG keeps its content type" "image/png" \
  "$(curl -s -o /dev/null -w '%{content_type}' "$BASE/media/assets/pic.png")"

curl -s "$BASE/media/assets/pic.png" -o "$WORK/served.png"
if cmp -s "$WORK/served.png" "$CONTENT/assets/pic.png"; then
  ok "served bytes match the source file"
else
  bad "served bytes differ from the source file"
fi

check "a CJK file name is served" "200" \
  "$(status "$BASE/media/assets/%E6%88%91%E7%9A%84%E5%9B%BE.png")"
check "a nested asset is served" "200" "$(status "$BASE/media/notes/nested.assets/diagram.png")"

etag="$(curl -s -D- -o /dev/null "$BASE/media/assets/pic.png" | tr -d '\r' \
  | sed -n 's/^[Ee][Tt][Aa][Gg]: //p')"
if [[ -n "$etag" ]]; then
  check "ETag revalidation returns 304" "304" \
    "$(status -H "If-None-Match: $etag" "$BASE/media/assets/pic.png")"
else
  bad "no ETag was returned"
fi

echo
echo "the media tree is not browsable"
check "a Markdown source is not downloadable" "404" "$(status "$BASE/media/assets/source.md")"
check "path traversal upward is refused" "404" \
  "$(status --path-as-is "$BASE/media/../site.db")"
check "path traversal out of the root is refused" "404" \
  "$(status --path-as-is "$BASE/media/assets/../../etc/hosts")"
check "a dotfile is refused" "404" "$(status "$BASE/media/.hidden")"

echo
echo "asset URL rewriting"
article="$(body "$BASE/posts/hello")"
check_contains "a relative image was repointed at /media" 'src="/media/assets/pic.png"' "$article"
check_contains "an image in raw HTML was repointed too" 'src="/media/assets/pic.png"' "$article"
check_contains "a CJK image name was percent-encoded" \
  'src="/media/assets/%E6%88%91%E7%9A%84%E5%9B%BE.png"' "$article"
check_contains "a remote image was left alone" 'src="https://example.com/x.png"' "$article"
check_contains "the rest of a rewritten tag survived" 'zoom:50%' "$article"
check_contains "a nested document resolves against its own directory" \
  'src="/media/notes/nested.assets/diagram.png"' "$(body "$BASE/posts/nested")"

# ---------------------------------------------------------------- reload
echo
echo "publishing again reloads without a restart"
cat >> "$CONTENT/2026-01-05-hello.md" <<'EOF'

## 热更新

这一行是重新发布之后加的。
EOF

"$ADMIN" build --content-dir "$CONTENT" --out "$DIST" \
  --site-url https://blog.test.example --title 内容目录测试 >/dev/null 2>&1

reloaded=0
for _ in $(seq 1 40); do
  if body "$BASE/posts/hello" | grep -q '这一行是重新发布之后加的'; then
    reloaded=1
    break
  fi
  sleep 0.5
done

if [[ "$reloaded" -eq 1 ]]; then
  ok "the new package was picked up automatically"
else
  bad "the server did not reload the new package"
fi

check_contains "the reload is visible in the log" "reloaded the content package" \
  "$(cat "$WORK/server.log")"

# ---------------------------------------------------------------- bundle
echo
echo "bundle"
# The data directory is deliberately not passed: it must be derived from the
# install root, which the next checks confirm.
"$ADMIN" bundle \
  --out "$DIST" \
  --domain blog.test.example \
  --install-dir /srv/tblogs >"$WORK/bundle.log" 2>&1
check "tblogs-admin bundle exits 0" "0" "$?"

for file in tblogs-server install.sh tblogs.service nginx.conf Caddyfile \
            cloudflared-config.yml ddns-update.sh check-exposure.sh README.md; do
  check "bundle contains $file" "yes" \
    "$([[ -f "$DIST/server/$file" ]] && echo yes || echo no)"
done

check "the bundled binary is executable" "yes" \
  "$([[ -x "$DIST/server/tblogs-server" ]] && echo yes || echo no)"
check "install.sh is executable" "yes" \
  "$([[ -x "$DIST/server/install.sh" ]] && echo yes || echo no)"

check_contains "the data directory is derived from the install root" \
  "TBLOGS_DATA_DIR=/srv/tblogs/data" "$(cat "$DIST/server/tblogs.service")"
check_contains "the unit keeps the filesystem read-only" "ProtectSystem=strict" \
  "$(cat "$DIST/server/tblogs.service")"
check_missing "no workspace placeholder survives in the unit" "/opt/tblogs" \
  "$(cat "$DIST/server/tblogs.service")"
check_contains "nginx is rendered for the domain" "blog.test.example" \
  "$(cat "$DIST/server/nginx.conf")"
check_contains "install.sh uses the requested root" 'ROOT="/srv/tblogs"' \
  "$(cat "$DIST/server/install.sh")"
check_missing "no media marker survives in install.sh" "MEDIA_DIR_MARKER" \
  "$(cat "$DIST/server/install.sh")"

for script in install.sh ddns-update.sh check-exposure.sh; do
  if bash -n "$DIST/server/$script" 2>/dev/null; then
    ok "$script is valid shell"
  else
    bad "$script has a syntax error"
  fi
done

# ---------------------------------------------------------------- export
echo
echo "export"
# A small page size so pagination is actually exercised: the static export has
# to move the page number into the path, and that is the part most likely to
# rot silently.
export_output="$("$ADMIN" export --out "$DIST" --site-dir "$DIST/site" --page-size 2 2>&1)"
check "tblogs-admin export exits 0" "0" "$?"
SITE="$DIST/site"

for produced in index.html 404.html feed.xml tags.html static/style.css \
                media/assets/pic.png posts/hello.html; do
  if [[ -f "$SITE/$produced" ]]; then
    ok "static export contains $produced"
  else
    bad "static export is missing $produced (\n$export_output)"
  fi
done

# A draft must not exist as a file at all, not merely redirect.
if [[ -f "$SITE/posts/draft.html" ]]; then
  bad "a draft was given a static page"
else
  ok "a draft was given no static page"
fi

# Pagination reached the path, and the query form is gone.
if [[ -f "$SITE/page/2.html" ]]; then
  ok "page 2 was written as page/2.html"
else
  bad "page/2.html is missing"
fi
check_contains "pagination links use a path" 'href="/page/2"' "$(cat "$SITE/index.html")"
check_missing "pagination emits no query string" '?page=' "$(cat "$SITE/index.html")"

# 404.html is not cosmetic: Cloudflare Pages treats a project without one as a
# single-page app and answers unknown paths with the home page.
check_contains "the 404 page is a real page" "没有这个页面" "$(cat "$SITE/404.html")"

# Every internal link must resolve to a file, using the host's lookup order:
# exact, then .html, then /index.html.
link_report="$(python3 - "$SITE" <<'PYEOF'
import html, pathlib, re, sys, urllib.parse
root = pathlib.Path(sys.argv[1])
link = re.compile(r'(?:href|src)="([^"]+)"')
def resolve(path):
    rel = urllib.parse.unquote(path).lstrip("/") or "index.html"
    for candidate in (root / rel, root / (rel + ".html"), root / rel / "index.html"):
        if candidate.is_file():
            return candidate
    return None
broken = []
for page in root.rglob("*.html"):
    for raw in link.findall(page.read_text(errors="replace")):
        if raw.startswith(("http://", "https://", "//", "mailto:", "data:", "#")):
            continue
        target = html.unescape(raw).split("#")[0].split("?")[0]
        if target and resolve(target) is None:
            broken.append(f"{page.relative_to(root)} -> {raw}")
print(len(broken))
for entry in broken[:5]:
    print(entry)
PYEOF
)"
check "every internal link in the static export resolves" "0" "$(printf '%s' "$link_report" | head -1)"
if [[ "$(printf '%s' "$link_report" | head -1)" != "0" ]]; then
  printf '%s\n' "$link_report" | tail -n +2 >&2
fi

echo
if [[ "$FAIL" -eq 0 ]]; then
  printf '\033[32m%d checks passed\033[0m\n' "$PASS"
  exit 0
fi

printf '\033[31m%d passed, %d FAILED\033[0m\n' "$PASS" "$FAIL"
exit 1
