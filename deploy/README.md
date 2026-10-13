# 部署与公网接入

这个目录回答一个问题：**怎么让本机上的 tblogs 被公网访问？**

先给结论：`DDNS` 只是整件事里很小的一块。而且**要先分清走 IPv4 还是 IPv6**——
这两条路的障碍完全不同，混在一起想是绝大多数人卡住的原因。

---

## 一、先搞清楚 DDNS 到底解决什么

常见误解是「DDNS = 内网穿透」。不是。

公网访问需要三个条件**同时**成立：

| # | 条件 | DDNS 能解决吗 |
|---|------|---------------|
| 1 | 你有一个**属于你**的公网 IP，且它在变 | ✅ 这正是 DDNS 唯一的用途 |
| 2 | 你能在**持有该公网 IP 的设备上**放行入站 | ❌ 不能 |
| 3 | 运营商没有封禁该端口 | ❌ 不能 |

DDNS 只做一件事：**定时把域名解析改写成当前 IP**。它不会替你打开防火墙，
也不会让运营商 NAT 后面的机器变得可达。

所以顺序是：**先确认 2 和 3，最后才配 1**。很多人反过来做，配好 DDNS 才发现
端口根本转发不了。

---

## 二、IPv4 和 IPv6 是两条不同的路

这是关键，务必分开看：

| | IPv4 | IPv6 |
|---|---|---|
| 有 NAT 吗 | 有，常叠加运营商 CGNAT | **没有 NAT** |
| 需要端口转发吗 | 需要 | 不需要，放行入站即可 |
| 地址够用吗 | 公网地址稀缺，家宽多为 CGNAT | 每个设备都有全局地址 |
| 常见障碍 | 运营商 CGNAT、封 80/443 | 路由器默认丢弃入站 IPv6 |
| 访客覆盖 | 所有人 | 仅 IPv6 访客（IPv4 访客需 Cloudflare 代理在前端） |

**IPv6 没有 NAT** 这一点很重要：只要路由器放行入站，你机器上的全局 IPv6
地址本身就能被公网直接访问，不需要任何端口映射，也不受运营商 CGNAT 影响。

代价是 IPv4-only 的访客访问不到。解决办法是在前面套一层 Cloudflare 代理
（用 AAAA 记录 + `proxied: true`），由 Cloudflare 接受 IPv4 请求并回源到
你的 IPv6 地址。

---

## 三、本机实测到的两种情况

`deploy/check-exposure.sh` 会重新测一遍。实测中遇到过两种完全不同的网络：

### 情况一：公司办公网络（技术上走不通）

| 观察 | 含义 |
|------|------|
| 本机 `172.28.240.75` | 私有大网段，前面至少一层 NAT |
| DNS 搜索域 `gjzq.root.ad` | 加入了企业 Active Directory 域 |
| DNS `192.168.15.22/23` | 企业内网 DNS |
| PAC 代理 `127.0.0.1:33331` | 企业统一上网代理 |
| 出口 `180.167.72.252` | AS4812 中国电信——**整栋楼共用的 NAT 出口** |

出口 IP 属于公司的设备，不属于你。你没有配置权限，企业防火墙也默认丢弃
外部主动发起的入站连接。此外，在办公电脑上对公网开放服务通常直接违反
公司安全策略。**这条路不要走。**

### 情况二：家用宽带（有戏，走 IPv6）

| 观察 | 含义 |
|------|------|
| 本机 `192.168.1.115`，网关 `192.168.1.1` | 典型家用路由器网段 |
| 公网 IPv4 `183.192.102.238` | 中国移动，**很可能是运营商 CGNAT 共享出口** |
| 全局 IPv6 `2409:8a1e:9895:6a50:1418:...` | ✅ 本机持有全局 IPv6 地址 |
| IPv4 国际出网 | ❌ 不可达（国内站点正常，境外 IPv4 不通） |
| IPv6 国际出网 | ✅ 正常 |

结论：**这台机器最有希望的路径是 IPv6 + AAAA 记录**，而不是传统的
「IPv4 端口转发 + DDNS」。

---

## 四、三条可行路线

### 路线 A：部署到云主机（最省心，推荐长期方案）

不依赖你的网络环境，域名直接指向云主机固定 IP。

```bash
# 方式一：直接在服务器上编译（最省事，推荐）
#   把仓库放到服务器，然后 cargo build --release

# 方式二：本机交叉编译 Linux 版本。
#   注意 macOS → Linux 还需要交叉链接器，例如：
#     brew tap messense/macos-cross-toolchains
#     brew install x86_64-unknown-linux-gnu
#     export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-unknown-linux-gnu-gcc
rustup target add x86_64-unknown-linux-gnu
cargo build --release --target x86_64-unknown-linux-gnu

# 只需上传两样：二进制 + 迁移目录。
# 模板在编译期编入二进制，CSS 用 include_str! 内嵌，都不需要随行。
rsync -av target/x86_64-unknown-linux-gnu/release/tblogs \
          migrations \
          user@your-vps:/opt/tblogs/

# 服务器上装服务
sudo cp deploy/tblogs.service /etc/systemd/system/
sudo install -m 600 /dev/null /etc/tblogs.env
echo 'TBLOGS_ADMIN_TOKEN='"$(openssl rand -hex 16)" | sudo tee /etc/tblogs.env
sudo systemctl enable --now tblogs

# 反向代理 + HTTPS
sudo cp deploy/nginx.conf /etc/nginx/sites-available/tblogs
sudo certbot --nginx -d blog.example.com
```

国内云主机用 80/443 对外提供 Web 服务需要 ICP 备案。

### 路线 B：反向隧道（不碰路由器，当前最省事）

由内网机器**主动外连**到公网中转，中转把请求回灌。完全绕过 NAT、CGNAT
和入站防火墙——**不需要公网 IP，不需要端口转发，也不需要 DDNS**。

Cloudflare Tunnel 在这台机器上尤其合适：它走 IPv6 出站（实测可用），
自带 HTTPS，并且 Cloudflare 在前端同时提供 IPv4 与 IPv6 入口。

```bash
brew install cloudflared
cloudflared tunnel login
cloudflared tunnel create tblogs
cloudflared tunnel route dns tblogs blog.example.com
cloudflared tunnel --config deploy/cloudflared-config.yml run tblogs
```

frp 版本（需要一个有公网 IP 的中转机，国内常用）：

```ini
# frpc.ini
[common]
server_addr = 你的中转机IP
server_port = 7000
token = <换成随机串>

[tblogs]
type = http
local_ip = 127.0.0.1
local_port = 8080
custom_domains = blog.example.com
```

> 注意：如果是在公司电脑上跑，技术可行 ≠ 合规可行，政策问题依然存在。

### 路线 C：自有网络直连（DDNS 的正统用法）

前提是**你拥有那台路由器**。分两种情况，先做 `check-exposure.sh` 判断属于哪种。

#### C1. IPv6 直连（本机最有希望的路径）

1. 确认有全局 IPv6 地址（`check-exposure.sh` 会打印）。macOS 上要挑
   **非 temporary** 的那一个——临时隐私地址会轮换，写进 DNS 会立刻失效。
2. **在路由器上放行入站 IPv6。** 家用路由器默认丢弃所有入站 IPv6 流量，
   这是最常见的卡点。规则形如：

   ```
   允许  入站  TCP  目标 <本机稳定IPv6>  端口 8080
   ```

   建议给这台机器在路由器上做 IPv6 地址/DHCPv6 绑定，否则地址一变，规则就失效。
3. 从手机流量（不要连家里 WiFi）访问 `http://[<你的IPv6>]:8080/healthz`，
   返回 `ok` 才算通。
4. 最后配 DDNS，用 **AAAA** 记录：

   ```bash
   export CF_API_TOKEN=...      # 权限 Zone.DNS Edit
   export CF_ZONE_ID=...
   export CF_RECORD_NAME=blog.example.com
   export CF_RECORD_TYPE=AAAA
   ./deploy/ddns-update.sh
   ```

   脚本默认读取**本机稳定的全局 IPv6**（而不是出站时用的临时地址），
   IP 没变就不写 API。放到 cron 每 5 分钟一次：

   ```cron
   */5 * * * * CF_API_TOKEN=... CF_ZONE_ID=... CF_RECORD_NAME=blog.example.com \
               CF_RECORD_TYPE=AAAA /opt/tblogs/deploy/ddns-update.sh \
               >> /var/log/tblogs-ddns.log 2>&1
   ```

   或用 `deploy/ddns.timer`。

5. **想让 IPv4 访客也能访问**，就把记录改成 Cloudflare 代理模式：

   ```bash
   CF_PROXIED=true ./deploy/ddns-update.sh
   ```

   这样 Cloudflare 接受 IPv4 请求并通过 IPv6 回源。

#### C2. IPv4 直连（传统做法，先确认不是 CGNAT）

1. **确认你有公网 IPv4。** 登录路由器看 WAN 口地址，与 `check-exposure.sh`
   打印的出口 IPv4 对比：
   - 相同 → ✅ 有公网 IP，继续
   - `100.64.x` ~ `100.127.x` → 运营商 CGNAT，**没有公网 IPv4**，请走 A / B / C1
   - `10.x` / `172.16-31.x` / `192.168.x` → 光猫还在做 NAT，需申请公网 IP 或改桥接
2. 路由器上把 WAN 的端口转发到本机，例如 `8080 → 192.168.1.115:8080`。
   建议给本机设静态 IP 或 DHCP 保留。
3. 从外网实测 `http://<公网IP>:8080/healthz`。家宽常见封 80/443。
4. 配 DDNS（默认就是 A 记录）：

   ```bash
   CF_RECORD_TYPE=A ./deploy/ddns-update.sh
   ```

5. 启动服务时绑定到所有地址：

   ```bash
   TBLOGS_BIND=0.0.0.0:8080 \
   TBLOGS_SITE_URL=https://blog.example.com \
   TBLOGS_ADMIN_TOKEN=$(openssl rand -hex 16) \
   ./tblogs
   ```

---

## 五、一个容易踩的坑：DDNS 脚本够不到 DNS 服务商 API

实测在这台机器所在的网络上，`api.cloudflare.com` **IPv4 和 IPv6 都不通**
（同一时刻 `api.github.com` 走 IPv6 正常）。也就是说脚本逻辑本身没问题，
但请求根本发不出去。先自测：

```bash
curl -s -o /dev/null -w '%{http_code}\n' \
  https://api.cloudflare.com/client/v4/user/tokens/verify
```

- 返回 **400** → 通了（没带 token 时 Cloudflare 就回 400），可以继续
- 超时 / 000 → 被墙或被网络限制，需要下面三种办法之一

三种解决办法：

1. **走本机代理。** 这台机器的 PAC（`http://127.0.0.1:33331/commands/pac`）
   指向 `127.0.0.1:7897`，实测可用：

   ```bash
   export https_proxy=http://127.0.0.1:7897
   export HTTPS_PROXY=http://127.0.0.1:7897
   ./deploy/ddns-update.sh
   ```

   curl 会自动读取这两个变量，cron 里也要带上。

2. **在能访问 API 的机器上跑 DDNS**，例如你的 VPS 定时更新解析记录，
   而不是本机。

3. **换成国内可直连的 DNS 服务商**：阿里云解析、DNSPod、华为云 DNS，
   或家用路由器自带的 DDNS（花生壳等）。这些 API 在国内可直连，
   是家宽场景更省事的选择。

---

## 六、判断自己属于哪种情况

```bash
./deploy/check-exposure.sh
```

它会分别打印 IPv4 与 IPv6 的本机地址、网关、出口地址、DNS 域、代理设置，
并给出判断。它**无法**替你确认入站是否真的通——那需要在路由器上放行，
并从外网（手机流量）实测。

---

## 七、上公网前必须做的事

无论走哪条路线，暴露到公网之前请逐项确认：

- [ ] **设置 `TBLOGS_ADMIN_TOKEN`**。未设置时 `/admin` 完全开放，任何人都能
      发文章、删文章。
- [ ] **`TBLOGS_SITE_URL` 用 `https://`**。程序据此决定会话 Cookie 是否加
      `Secure` 标志。
- [ ] **启用 HTTPS**（`deploy/nginx.conf` 或 `deploy/Caddyfile`）。明文传输
      管理员令牌等于公开口令。用 Cloudflare Tunnel / 代理时 HTTPS 由前端提供。
- [ ] **给 `/admin/login` 限速**。当前没有防暴力破解，nginx 配置里已给出
      `limit_req` 示例。
- [ ] **考虑把 `/admin` 限制到内网/VPN**，而不是对整个公网开放。
- [ ] **备份 `data/tblogs.db`**。SQLite 开了 WAL，热备份请连同 `-wal`、`-shm`
      一起复制，或先停服务再复制。
- [ ] **国内云主机别忘了 ICP 备案**。

---

## 八、本目录文件

| 文件 | 用途 |
|------|------|
| `check-exposure.sh` | 分 IPv4/IPv6 诊断当前网络能否做入站 |
| `ddns-update.sh` | Cloudflare DDNS 更新（支持 A / AAAA，幂等） |
| `ddns.service` / `ddns.timer` | 用 systemd 定时跑 DDNS |
| `tblogs.service` | systemd 服务单元 |
| `nginx.conf` | nginx 反向代理 + HTTPS + 限速 |
| `Caddyfile` | Caddy 版（自动申请证书，配置更短） |
| `cloudflared-config.yml` | Cloudflare Tunnel 配置 |
