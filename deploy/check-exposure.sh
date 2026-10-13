#!/usr/bin/env bash
#
# Diagnose whether this machine can plausibly be reachable from the internet.
#
# Reports the facts it can observe from inside and from outside, then says what
# those facts imply. It CANNOT confirm that inbound traffic actually arrives —
# that needs router access plus a probe from a different network. The decisive
# manual tests are printed at the end.
#
# IPv4 and IPv6 are treated separately on purpose: they are different paths
# with different obstacles, and a host often has a usable address on only one.

set -uo pipefail

# Domestic IPv4 echo services first: on mainland networks foreign IPv4 is
# frequently unreachable while IPv6 to the same services still works.
IPV4_SERVICES_DOMESTIC="https://myip.ipip.net https://ip.3322.net https://ipv4.ip.sb"
IPV4_SERVICES_FOREIGN="https://api.ipify.org https://ipv4.icanhazip.com"
IPV6_SERVICES="https://api64.ipify.org https://ipv6.icanhazip.com"

have() { command -v "$1" >/dev/null 2>&1; }

# Pull the first IPv4-looking token out of a response body.
first_ipv4() { grep -oE '([0-9]{1,3}\.){3}[0-9]{1,3}' | head -1; }
# Pull the first IPv6-looking token out of a response body.
first_ipv6() { grep -oE '([0-9a-fA-F]{1,4}:){2,}[0-9a-fA-F]{0,4}' | head -1; }

echo "tblogs exposure check"
echo "====================="
echo

# ---------------------------------------------------------------- local addresses
echo "本机地址"
if have ipconfig; then
  for iface in $(ifconfig -l 2>/dev/null); do
    v4=$(ipconfig getifaddr "$iface" 2>/dev/null) || continue
    [[ -n "$v4" ]] && printf '  %-6s IPv4  %s\n' "$iface" "$v4"
  done
elif have ip; then
  while read -r addr iface; do
    printf '  %-6s IPv4  %s\n' "$iface" "$addr"
  done < <(ip -o -4 addr show 2>/dev/null | awk '{split($4,a,"/"); print a[1], $2}')
fi

# Globally routable IPv6 (2000::/3). Link-local fe80:: and the rotating
# temporary privacy addresses are both excluded.
global_v6=""
if have ifconfig; then
  while read -r addr; do
    printf '  %-6s IPv6  %s\n' "en0" "$addr"
    [[ -z "$global_v6" ]] && global_v6="$addr"
  done < <(ifconfig 2>/dev/null | awk '
      /inet6/ {
        addr = $2
        sub(/%.*/, "", addr)                 # drop the zone id
        split(addr, h, ":")
        # Keep only 2000::/3 (global unicast). This drops fe80:: link-local,
        # fd00::/8 unique-local, and the rotating temporary privacy addresses.
        if (h[1] ~ /^[23][0-9a-fA-F]{3}$/) print addr
      }' | sort -u)
elif have ip; then
  while read -r addr iface; do
    printf '  %-6s IPv6  %s\n' "$iface" "$addr"
    [[ -z "$global_v6" ]] && global_v6="$addr"
  done < <(ip -o -6 addr show scope global 2>/dev/null | awk '{split($4,a,"/"); print a[1], $2}')
fi
[[ -z "$global_v6" ]] && echo "  (没有全局 IPv6 地址)"
echo

# ---------------------------------------------------------------- gateway
echo "默认网关"
if have netstat; then
  v4gw=$(netstat -rn -f inet 2>/dev/null | awk '/^default/{print $2; exit}')
  v6gw=$(netstat -rn -f inet6 2>/dev/null | awk '/^default/{print $2; exit}')
  [[ -n "$v4gw" ]] && echo "  IPv4  $v4gw"
  [[ -n "$v6gw" ]] && echo "  IPv6  $v6gw"
fi
echo

# ---------------------------------------------------------------- egress addresses
echo "出口 IP（从外部服务观测）"
public_v4=""
for svc in $IPV4_SERVICES_DOMESTIC $IPV4_SERVICES_FOREIGN; do
  body=$(curl -4 -fsS --max-time 8 "$svc" 2>/dev/null) || continue
  candidate=$(printf '%s' "$body" | first_ipv4)
  if [[ -n "$candidate" ]]; then
    public_v4="$candidate"
    printf '  IPv4  %s   (%s)\n' "$candidate" "$svc"
    break
  fi
done
[[ -z "$public_v4" ]] && echo "  IPv4  (不可达——可能是纯 IPv6 接入，或 IPv4 出网被限制)"

public_v6=""
for svc in $IPV6_SERVICES; do
  body=$(curl -6 -fsS --max-time 8 "$svc" 2>/dev/null) || continue
  candidate=$(printf '%s' "$body" | first_ipv6)
  if [[ -n "$candidate" ]]; then
    public_v6="$candidate"
    printf '  IPv6  %s   (%s)\n' "$candidate" "$svc"
    break
  fi
done
[[ -z "$public_v6" ]] && echo "  IPv6  (不可达)"

if [[ -n "$public_v4" ]] && have curl; then
  info=$(curl -4 -fsS --max-time 8 "https://ipinfo.io/$public_v4/json" 2>/dev/null || true)
  org=$(printf '%s' "$info" | sed -n 's/.*"org"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
  [[ -n "$org" ]] && echo "        IPv4 归属: $org"
fi
echo

# ---------------------------------------------------------------- environment tells
echo "网络环境特征"
corporate=0
weak_proxy=0

if have scutil; then
  domains=$(scutil --dns 2>/dev/null | sed -n 's/.*search domain\[[0-9]*\][[:space:]]*:[[:space:]]*//p' | sort -u)
else
  domains=$(awk '/^search/{for (i=2; i<=NF; i++) print $i}' /etc/resolv.conf 2>/dev/null | sort -u)
fi

if [[ -n "$domains" ]]; then
  echo "  DNS 搜索域:"
  while IFS= read -r d; do
    [[ -z "$d" ]] && continue
    echo "    $d"
    case "$d" in *.ad|*.local|*.lan|*.corp|*.internal) corporate=1 ;; esac
  done <<<"$domains"
else
  echo "  DNS 搜索域: (无)"
fi

if have scutil; then
  pac=$(scutil --proxy 2>/dev/null | sed -n 's/.*ProxyAutoConfigURLString[[:space:]]*:[[:space:]]*//p')
  if [[ -n "$pac" ]]; then
    echo "  PAC 代理: $pac"
    # A PAC on loopback is usually a local desktop proxy tool, which says
    # nothing about the surrounding network. A remote PAC is a strong signal.
    case "$pac" in
      *127.0.0.1*|*localhost*) weak_proxy=1 ;;
      *) corporate=1 ;;
    esac
  fi
fi

if [[ "$weak_proxy" -eq 1 ]]; then
  echo "  （PAC 在 127.0.0.1，通常是本机代理工具，不作为企业网络判据）"
fi

gwv4=$(netstat -rn -f inet 2>/dev/null | awk '/^default/{print $2; exit}')
case "$gwv4" in
  192.168.0.*|192.168.1.*|192.168.31.*) echo "  网关 $gwv4 是典型家用路由器网段" ;;
  10.*|172.1[6-9].*|172.2[0-9].*|172.3[01].*) echo "  网关 $gwv4 是私有大网段，常见于企业网络" ;;
esac
echo

# ---------------------------------------------------------------- verdict
echo "判断"
echo "-----"

if [[ -n "$public_v4" ]]; then
  if [[ "$public_v4" =~ ^100\.(6[4-9]|[7-9][0-9]|1[01][0-9]|12[0-7])\. ]]; then
    echo "  ✗ IPv4 $public_v4 落在 100.64.0.0/10（运营商级 NAT）。"
    echo "    你没有公网 IPv4，IPv4 端口转发无从谈起。"
  else
    echo "  ? IPv4 $public_v4 是普通公网地址，但还需确认它是否属于「你的」路由器"
    echo "    ——运营商 CGNAT 的共享出口同样长这样。见下方对比方法。"
  fi
else
  echo "  ✗ IPv4 出网不可用，IPv4 路径不成立。"
fi

echo
if [[ -n "$global_v6" ]]; then
  echo "  ✓ 本机持有全局 IPv6 地址：$global_v6"
  echo "    IPv6 没有 NAT。只要路由器放行入站，这个地址本身就能被直接访问，"
  echo "    不需要端口转发，也不受运营商 CGNAT 影响。这是当前最有希望的路径。"
  echo "    注意：macOS 的 temporary 隐私地址会轮换，上面列出的稳定地址才适合写进 DNS。"
else
  echo "  ✗ 没有全局 IPv6 地址，IPv6 路径不成立。"
fi

if [[ "$corporate" -eq 1 ]]; then
  echo
  echo "  ⚠ 检测到企业网络特征（AD 域或远程 PAC）。"
  echo "    这类网络里出口公网 IP 属于公司设备，你既没有配置权限，"
  echo "    企业防火墙也默认丢弃外部入站连接；在此类机器上对外提供服务"
  echo "    通常违反公司安全策略，不建议。"
fi

cat <<'EOF'

必须手动确认的两件事
--------------------
[1] 这个出口 IP 是不是你的？（只对 IPv4 有意义）
    对比「上面打印的 IPv4」与「路由器 WAN 口地址」：
      • 相同                     → 你有公网 IPv4，可配端口转发 + DDNS
      • WAN 口是 100.64-100.127.x → 运营商 CGNAT，没有公网 IPv4
      • WAN 口是 10.x / 172.16-31.x / 192.168.x
                                 → 光猫仍有一层 NAT，需申请公网 IP 或改桥接

[2] 入站到底通不通？（IPv6 与 IPv4 必须分别测）
    用手机流量（不要连家里 WiFi）访问：
      IPv4:  http://<上面那个IPv4>:8080/healthz
      IPv6:  http://[<上面那个IPv6>]:8080/healthz
    返回 ok 才算真的通。IPv6 不通通常是路由器默认丢弃入站 IPv6，需要显式放行；
    家宽还常见封 IPv4 的 80/443。

走哪条路
--------
  • 有全局 IPv6 且路由器能放行入站 → IPv6 + AAAA 记录 DDNS（README 路线 C）
    缺点：IPv4-only 的访客访问不到，需要 Cloudflare 代理在前端补上 IPv4
  • 不想碰路由器 / 没有公网 IP     → Cloudflare Tunnel（README 路线 B）
    纯出站连接：无需入站放行、无需公网 IP、无需 DDNS，自带 HTTPS 与 IPv4 前端
  • 想要固定、最省心               → 部署到云主机（README 路线 A）
EOF
