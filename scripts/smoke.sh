#!/usr/bin/env bash
#
# End-to-end smoke test for tblogs.
#
# Exercises every route against a running instance, including the full admin
# create -> publish -> unpublish -> delete lifecycle. Exits non-zero if any
# check fails, so it can gate a deployment.
#
# Usage:
#   ./scripts/smoke.sh                      # against http://127.0.0.1:8080
#   BASE=http://127.0.0.1:9000 ./scripts/smoke.sh
#
# The admin lifecycle test creates a post and deletes it again; it is safe to
# run against a live instance, but it does write.

set -uo pipefail

BASE="${BASE:-http://127.0.0.1:8080}"
COOKIE_JAR="$(mktemp)"
PASS=0
FAIL=0

cleanup() { rm -f "$COOKIE_JAR"; }
trap cleanup EXIT

ok()   { printf '  \033[32mok\033[0m   %s\n' "$1"; PASS=$((PASS + 1)); }
bad()  { printf '  \033[31mFAIL\033[0m %s\n' "$1"; FAIL=$((FAIL + 1)); }

# check <description> <expected> <actual>
check() {
  if [[ "$2" == "$3" ]]; then ok "$1"; else bad "$1 (expected '$2', got '$3')"; fi
}

# check_contains <description> <needle> <haystack>
check_contains() {
  if [[ "$3" == *"$2"* ]]; then ok "$1"; else bad "$1 (missing '$2')"; fi
}

# check_missing <description> <needle> <haystack>
check_missing() {
  if [[ "$3" != *"$2"* ]]; then ok "$1"; else bad "$1 (unexpectedly found '$2')"; fi
}

status() { curl -s -o /dev/null -w '%{http_code}' "$@"; }
body()   { curl -s "$@"; }

# Extract deduplicated hrefs matching a pattern from a page.
extract_hrefs() { grep -oE "href=\"$1\"" | sed -E 's/href="([^"]+)"/\1/' | sort -u; }

if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 is required to validate the feed XML" >&2
  exit 2
fi

echo "tblogs smoke test against $BASE"
echo

# ---------------------------------------------------------------- reachability
echo "reachability"
hc=$(status "$BASE/healthz")
check "GET /healthz -> 200" "200" "$hc"
check_contains "GET /healthz body -> ok" "ok" "$(body "$BASE/healthz")"

# ---------------------------------------------------------------- public pages
echo
echo "public pages"
check "GET / -> 200" "200" "$(status "$BASE/")"

index_html=$(body "$BASE/")
check_contains "home lists a post" "你好，世界" "$index_html"
check_contains "home links the stylesheet" "/static/style.css" "$index_html"
check_contains "home links the feed" "/feed.xml" "$index_html"

check "GET /static/style.css -> 200" "200" "$(status "$BASE/static/style.css")"
check "GET /static/missing.css -> 404" "404" "$(status "$BASE/static/missing.css")"

echo
echo "post links round-trip"
# Every link the index emits must resolve. This is the strongest available
# check on slug generation: a CJK title yields a CJK slug, which the template
# percent-encodes and the router has to decode again before matching.
post_link_count=$(printf '%s' "$index_html" | extract_hrefs '/posts/[^"]+' | wc -l | tr -d ' ')
if [[ "$post_link_count" -gt 0 ]]; then
  ok "index emits $post_link_count post link(s)"
else
  bad "index emitted no post links"
fi

renders_code=0
while IFS= read -r href; do
  [[ -z "$href" ]] && continue
  page=$(body "$BASE$href")
  check "GET $href -> 200" "200" "$(status "$BASE$href")"
  check_contains "GET $href renders a heading" "<h1>" "$page"
  [[ "$page" == *"<pre><code"* ]] && renders_code=1
done < <(printf '%s' "$index_html" | extract_hrefs '/posts/[^"]+')

if [[ "$renders_code" -eq 1 ]]; then
  ok "a post renders a fenced code block"
else
  bad "no post rendered a fenced code block"
fi

echo
echo "tags"
check "GET /tags -> 200" "200" "$(status "$BASE/tags")"
tags_html=$(body "$BASE/tags")
check_contains "tag cloud lists Rust" "Rust" "$tags_html"
check "GET /tags/rust -> 200" "200" "$(status "$BASE/tags/rust")"
check "GET /tags/nope -> 404" "404" "$(status "$BASE/tags/nope")"

# Tag names include CJK, so their links have to round-trip the same way.
tag_link_count=$(printf '%s' "$index_html" | extract_hrefs '/tags/[^"]+' | wc -l | tr -d ' ')
if [[ "$tag_link_count" -gt 0 ]]; then
  ok "index emits $tag_link_count tag link(s)"
else
  bad "index emitted no tag links"
fi

while IFS= read -r href; do
  [[ -z "$href" ]] && continue
  check "GET $href -> 200" "200" "$(status "$BASE$href")"
done < <(printf '%s' "$index_html" | extract_hrefs '/tags/[^"]+')

echo
echo "feed"
check "GET /feed.xml -> 200" "200" "$(status "$BASE/feed.xml")"
feed=$(body "$BASE/feed.xml")
check_contains "feed declares rss 2.0" '<rss version="2.0"' "$feed"
check_contains "feed has items" "<item>" "$feed"
# RSS readers require absolute URLs; they resolve against the feed's own
# location, so a relative link silently breaks every subscriber.
check_contains "feed items use absolute links" "<link>http" "$feed"
check_contains "feed carries pubDate" "<pubDate>" "$feed"
check_contains "feed carries a self link" 'rel="self"' "$feed"
if command -v python3 >/dev/null 2>&1; then
  if printf '%s' "$feed" | python3 -c 'import sys,xml.dom.minidom as m; m.parseString(sys.stdin.read())' 2>/dev/null; then
    ok "feed is well-formed XML"
  else
    bad "feed is well-formed XML"
  fi
fi

echo
echo "not found"
check "GET /posts/does-not-exist -> 404" "404" "$(status "$BASE/posts/does-not-exist")"
check "GET /no-such-route -> 404" "404" "$(status "$BASE/no-such-route")"

# ---------------------------------------------------------------- admin
echo
echo "admin authentication"
# Admin requests carry the session cookie. Against an ungated instance the jar
# stays empty and the flag is a no-op, so one script covers both modes; set
# ADMIN_TOKEN to exercise the gate itself.
admin_status() { curl -s -o /dev/null -w '%{http_code}' -b "$COOKIE_JAR" "$@"; }
admin_body()   { curl -s -b "$COOKIE_JAR" "$@"; }

if [[ -n "${ADMIN_TOKEN:-}" ]]; then
  check "GET /admin without a session -> 303" "303" "$(status "$BASE/admin")"
  check "POST /admin/login with a wrong token -> 401" "401" \
    "$(status -X POST "$BASE/admin/login" --data-urlencode "token=definitely-not-the-token")"
  check "POST /admin/login with the right token -> 303" "303" \
    "$(status -c "$COOKIE_JAR" -X POST "$BASE/admin/login" --data-urlencode "token=$ADMIN_TOKEN")"
  check "GET /admin with the session cookie -> 200" "200" "$(admin_status "$BASE/admin")"
  check "POST /admin/new without a session -> 303" "303" \
    "$(status -X POST "$BASE/admin/new" --data-urlencode "title=Nope" --data-urlencode "status=draft")"

  # Logout is verified in its own jar so it cannot disturb the session the
  # lifecycle checks below depend on.
  logout_jar=$(mktemp)
  curl -s -c "$logout_jar" -o /dev/null -X POST "$BASE/admin/login" \
    --data-urlencode "token=$ADMIN_TOKEN"
  check "POST /admin/logout -> 303" "303" \
    "$(curl -s -o /dev/null -w '%{http_code}' -b "$logout_jar" -c "$logout_jar" -X POST "$BASE/admin/logout")"
  check "session is cleared after logout -> 303" "303" \
    "$(curl -s -o /dev/null -w '%{http_code}' -b "$logout_jar" "$BASE/admin")"
  rm -f "$logout_jar"
else
  echo "  (skipped: set ADMIN_TOKEN to exercise the auth gate)"
fi

echo
echo "admin lifecycle"
check "GET /admin -> 200" "200" "$(admin_status "$BASE/admin")"

created=$(admin_status -X POST "$BASE/admin/new" \
  --data-urlencode "title=Smoke Test Post" \
  --data-urlencode "slug=" \
  --data-urlencode "summary=" \
  --data-urlencode "body_md=# Hello

Smoke **body** text." \
  --data-urlencode "tags=smoke, test" \
  --data-urlencode "status=published")
check "POST /admin/new -> 303" "303" "$created"

admin_html=$(admin_body "$BASE/admin")
check_contains "new post appears in admin list" "Smoke Test Post" "$admin_html"
check_contains "tags were attached" "smoke" "$admin_html"

# The list is ordered by updated_at DESC, so the new post is the first row.
post_id=$(printf '%s' "$admin_html" | grep -oE '/admin/posts/[0-9]+/edit' | head -1 | grep -oE '[0-9]+' || true)

if [[ -z "$post_id" ]]; then
  bad "could not determine the new post id"
else
  ok "resolved new post id ($post_id)"

  check "published post is publicly reachable" "200" "$(status "$BASE/posts/smoke-test-post")"
  check_contains "rendered markdown from admin form" "<h1>Hello</h1>" "$(body "$BASE/posts/smoke-test-post")"
  check "GET edit form -> 200" "200" "$(admin_status "$BASE/admin/posts/$post_id/edit")"

  check "POST toggle -> 303" "303" "$(admin_status -X POST "$BASE/admin/posts/$post_id/publish")"
  check "unpublished post is hidden" "404" "$(status "$BASE/posts/smoke-test-post")"
  check "draft still visible in admin" "200" "$(admin_status "$BASE/admin")"

  check "re-publish -> 303" "303" "$(admin_status -X POST "$BASE/admin/posts/$post_id/publish")"
  check "re-published post is public again" "200" "$(status "$BASE/posts/smoke-test-post")"

  check "POST delete -> 303" "303" "$(admin_status -X POST "$BASE/admin/posts/$post_id/delete")"
  check "deleted post is gone" "404" "$(status "$BASE/posts/smoke-test-post")"
  check_missing "deleted post left the admin list" "Smoke Test Post" "$(admin_body "$BASE/admin")"
  check "empty tag was cleaned up" "404" "$(status "$BASE/tags/smoke")"
fi

echo
echo "validation"
check "empty title is rejected" "200" "$(admin_status -X POST "$BASE/admin/new" \
  --data-urlencode "title=" --data-urlencode "body_md=x" --data-urlencode "status=draft")"
check_contains "empty title shows a message" "标题不能为空" "$(admin_body -X POST "$BASE/admin/new" \
  --data-urlencode "title=" --data-urlencode "body_md=x" --data-urlencode "status=draft")"

# ---------------------------------------------------------------- summary
echo
if [[ "$FAIL" -eq 0 ]]; then
  printf '\033[32m%d checks passed\033[0m\n' "$PASS"
  exit 0
fi

printf '\033[31m%d passed, %d FAILED\033[0m\n' "$PASS" "$FAIL"
exit 1
