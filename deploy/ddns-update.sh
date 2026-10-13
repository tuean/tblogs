#!/usr/bin/env bash
#
# Point a Cloudflare DNS record (A or AAAA) at this machine's current address.
#
# Idempotent: when the record already matches it exits 0 without issuing a
# write, so it is safe to run from cron every few minutes.
#
# Required environment:
#   CF_API_TOKEN     API token with "Zone.DNS Edit" on the zone
#   CF_ZONE_ID       zone id (Cloudflare dashboard -> your zone -> API section)
#   CF_RECORD_NAME   e.g. blog.example.com
#
# Optional environment:
#   CF_RECORD_TYPE   A (default) or AAAA. Use AAAA for the IPv6 path.
#   CF_IP_SOURCE     auto (default) | local | remote
#                    For AAAA, "auto" prefers the machine's own stable global
#                    IPv6 address over asking a remote echo service. This
#                    matters on macOS: an outbound IPv6 connection usually
#                    leaves from a rotating temporary privacy address, which
#                    must NOT be published in DNS.
#   CF_RECORD_TTL    seconds, default 120 (use 1 for "automatic")
#   CF_PROXIED       true/false, default false. Set true to hide the origin
#                    behind Cloudflare — also the way to give IPv4-only
#                    visitors access to an IPv6-only origin.
#   IPV4_SERVICE     default https://myip.ipip.net (domestic-first: foreign
#                    IPv4 is often unreachable from mainland networks)
#   IPV6_SERVICE     default https://api64.ipify.org
#
# Reaching the Cloudflare API:
#   On some mainland connections api.cloudflare.com is unreachable over both
#   IPv4 and IPv6. curl honours the standard proxy variables, so either export
#   them before running (cron included):
#     export https_proxy=http://127.0.0.1:7897
#     export HTTPS_PROXY=http://127.0.0.1:7897
#   or run this script from a host that can reach the API, such as your VPS.
#
#   Check reachability first:
#     curl -s -o /dev/null -w '%{http_code}\n' \
#       https://api.cloudflare.com/client/v4/user/tokens/verify
#   A 400 (no token supplied) means you reached Cloudflare. A timeout means the
#   API is blocked for you and a proxy or another host is required.
#
# Requires: curl, jq

set -euo pipefail

: "${CF_API_TOKEN:?set CF_API_TOKEN (Zone.DNS Edit)}"
: "${CF_ZONE_ID:?set CF_ZONE_ID}"
: "${CF_RECORD_NAME:?set CF_RECORD_NAME, e.g. blog.example.com}"

CF_RECORD_TYPE="${CF_RECORD_TYPE:-A}"
CF_IP_SOURCE="${CF_IP_SOURCE:-auto}"
CF_RECORD_TTL="${CF_RECORD_TTL:-120}"
CF_PROXIED="${CF_PROXIED:-false}"
IPV4_SERVICE="${IPV4_SERVICE:-https://myip.ipip.net}"
IPV6_SERVICE="${IPV6_SERVICE:-https://api64.ipify.org}"

for tool in curl jq; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "missing required tool: $tool" >&2
    exit 2
  fi
done

case "$CF_RECORD_TYPE" in
  A|AAAA) ;;
  *) echo "CF_RECORD_TYPE must be A or AAAA (got '$CF_RECORD_TYPE')" >&2; exit 2 ;;
esac

API="https://api.cloudflare.com/client/v4"
AUTH=(-H "Authorization: Bearer $CF_API_TOKEN" -H "Content-Type: application/json")

log() { printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*"; }

# The machine's own stable global IPv6, skipping link-local and the rotating
# temporary privacy addresses. Empty when there is no global IPv6.
local_stable_ipv6() {
  if command -v ifconfig >/dev/null 2>&1; then
    ifconfig 2>/dev/null \
      | awk '
          /inet6/ {
            addr = $2
            sub(/%.*/, "", addr)
            split(addr, h, ":")
            # 2000::/3 only: skips fe80:: link-local, fd00::/8 ULA, and the
            # rotating temporary privacy addresses.
            if (h[1] ~ /^[23][0-9a-fA-F]{3}$/) print addr
          }' \
      | sort -u | head -1
  else
    ip -o -6 addr show scope global 2>/dev/null \
      | awk '{split($4,a,"/"); print a[1]}' | head -1
  fi
}

# --- 1. determine the address -------------------------------------------------
ip=""
if [[ "$CF_RECORD_TYPE" == "AAAA" && "$CF_IP_SOURCE" != "remote" ]]; then
  ip="$(local_stable_ipv6 || true)"
  [[ -n "$ip" ]] && log "using local stable IPv6: $ip"
fi

if [[ -z "$ip" && "$CF_IP_SOURCE" != "local" ]]; then
  if [[ "$CF_RECORD_TYPE" == "AAAA" ]]; then
    ip="$(curl -6 -fsS --max-time 10 "$IPV6_SERVICE" | tr -d '[:space:]')"
  else
    ip="$(curl -4 -fsS --max-time 10 "$IPV4_SERVICE" \
      | grep -oE '([0-9]{1,3}\.){3}[0-9]{1,3}' | head -1)"
  fi
fi

if [[ -z "$ip" ]]; then
  log "ERROR: could not determine a $CF_RECORD_TYPE address for this host"
  exit 1
fi

# --- 2. validate it really is the right family --------------------------------
if [[ "$CF_RECORD_TYPE" == "AAAA" ]]; then
  if [[ "$ip" != *:* || ! "$ip" =~ ^[0-9a-fA-F:]+$ ]]; then
    log "ERROR: '$ip' is not an IPv6 address"
    exit 1
  fi
else
  if [[ ! "$ip" =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}$ ]]; then
    log "ERROR: '$ip' is not an IPv4 address"
    exit 1
  fi
fi

# --- 3. existing record -------------------------------------------------------
resp="$(curl -fsS --max-time 15 "${AUTH[@]}" \
  "$API/zones/$CF_ZONE_ID/dns_records?type=$CF_RECORD_TYPE&name=$CF_RECORD_NAME")"

if [[ "$(jq -r '.success' <<<"$resp")" != "true" ]]; then
  log "ERROR: lookup rejected: $(jq -c '.errors' <<<"$resp")"
  exit 1
fi

if [[ "$(jq -r '.result | length' <<<"$resp")" -eq 0 ]]; then
  log "ERROR: no $CF_RECORD_TYPE record named $CF_RECORD_NAME in zone $CF_ZONE_ID"
  log "       create the record once in the dashboard, then let this script maintain it"
  exit 1
fi

record_id="$(jq -r '.result[0].id' <<<"$resp")"
current="$(jq -r '.result[0].content' <<<"$resp")"

# --- 4. write only when the address actually changed --------------------------
if [[ "$current" == "$ip" ]]; then
  log "unchanged: $CF_RECORD_NAME $CF_RECORD_TYPE -> $ip"
  exit 0
fi

body="$(jq -nc --arg type "$CF_RECORD_TYPE" --arg name "$CF_RECORD_NAME" \
  --arg ip "$ip" --argjson ttl "$CF_RECORD_TTL" --argjson proxied "$CF_PROXIED" \
  '{type: $type, name: $name, content: $ip, ttl: $ttl, proxied: $proxied}')"

updated="$(curl -fsS --max-time 15 -X PUT "${AUTH[@]}" \
  --data "$body" "$API/zones/$CF_ZONE_ID/dns_records/$record_id")"

if [[ "$(jq -r '.success' <<<"$updated")" != "true" ]]; then
  log "ERROR: update rejected: $(jq -c '.errors' <<<"$updated")"
  exit 1
fi

log "updated: $CF_RECORD_NAME $CF_RECORD_TYPE $current -> $ip"
