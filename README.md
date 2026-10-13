# tblogs

一个用 Rust 写的博客系统，**拆成两个程序**：

| 程序 | 跑在哪 | 做什么 |
|------|--------|--------|
| `tblogs-admin` | 你的电脑 | 读本地 Markdown 目录 → 生成内容包 → 生成部署文件 → 发布到服务器 |
| `tblogs-server` | 服务器 | 只提供 HTTP 服务，**只读**，没有任何写作入口 |

写作完全在本地用 Typora（或任何编辑器）完成。服务器上不存在可以改内容的接口，
所以它被攻破也改不了你的文章。

技术栈：**axum** 提供 HTTP、**SQLite** 存内容、**askama** 服务端渲染、
**pulldown-cmark** 渲染 Markdown。没有前端构建，没有 Node，没有 CDN。

---

## 快速开始

```bash
cargo build --release

# 1. 看着写：指向你的 Markdown 目录，改一个文件浏览器里就更新
./target/release/tblogs-admin preview --content-dir ~/Documents/blog

# 2. 生成内容包 + 部署文件
./target/release/tblogs-admin build  --content-dir ~/Documents/blog --out dist \
    --site-url https://blog.example.com --title 我的博客
./target/release/tblogs-admin bundle --out dist --domain blog.example.com

# 3. 发布到服务器（需要事先配好 SSH 免密）
./target/release/tblogs-admin publish --out dist --to user@server:/srv/tblogs

# 4. 在服务器上装一次
ssh user@server 'sudo bash /srv/tblogs/server/install.sh'
```

之后每次写文章只要 `build` + `publish`；服务器会自己发现新的内容包并重新加载，
**不需要重启**。

---

## 管理端命令

### `build` — 把目录编译成内容包

```bash
tblogs-admin build --content-dir ~/blog --out dist \
  --site-url https://blog.example.com \
  --title 我的博客 --description '一句话介绍'
```

产出：

```
dist/
  site.db          # 文章、标签、站点设置
  media/           # 可发布的图片等资源，路径与你的目录一致
```

要点：

- **每次都从头构建。** 数据库是构建产物，不是长期存储，所以没有迁移链，
  服务器也不需要跑迁移。文章以 slug 为对外标识，重建不会改变 URL。
- **只镜像可发布的资源。** 扩展名不在白名单里的文件不会进包，尤其是 `.md`
  源文件永远不会被打包。
- **先写临时目录再原子替换。** 构建失败不会留下半个包，上一次的包保持可用。
- 构建完会把 WAL 合并并切成回滚日志，产出一个**单文件**数据库——
  这样服务器才能只读打开它（只读打开 WAL 数据库需要写 `-shm`，只读部署给不了）。

### `preview` — 本地预览

```bash
tblogs-admin preview --content-dir ~/blog --bind 127.0.0.1:8080
```

先构建，然后起来一个真正的服务端。默认**监听目录变化自动重建**，配合服务端的
自动重载，保存文件后刷新浏览器就能看到效果。用 `--no-watch` 关掉。

### `bundle` — 生成整套部署文件

```bash
tblogs-admin bundle --out dist --domain blog.example.com --install-dir /opt/tblogs
```

产出 `dist/server/`：

| 文件 | 用途 |
|------|------|
| `tblogs-server` | 服务端二进制（自动从旁边找，或用 `--server-binary` 指定） |
| `install.sh` | 在服务器上以 root 运行，一次装好 |
| `tblogs.service` | systemd 单元，已填好你的路径 |
| `nginx.conf` / `Caddyfile` | 反向代理 + HTTPS，二选一 |
| `ddns-update.sh` | Cloudflare DDNS 更新（支持 A / AAAA） |
| `check-exposure.sh` | 诊断这台机器能不能被公网访问 |
| `cloudflared-config.yml` | 反向隧道配置（无需公网 IP） |
| `tblogs.env.example` | 可选的环境变量覆盖 |

配置模板**编译在二进制里**，并按你给的域名和路径渲染好，没有留给你手改的占位符。
`--data-dir` 默认是 `<install-dir>/data`。

### `publish` — 发布到服务器

```bash
tblogs-admin publish --out dist --to user@server:/srv/tblogs --dry-run
```

先建远端目录（macOS 自带的 rsync 没有 `--mkpath`），再 `rsync --delete
--delay-updates` 整个包。`--delay-updates` 让 rsync 先传临时文件、最后统一改名，
所以运行中的服务器只会看到旧的或新的 `site.db`，不会读到写了一半的文件。

### `doctor` — 发布前自检

```bash
tblogs-admin doctor --content-dir ~/blog --to user@server:/srv/tblogs
```

检查目录、Markdown 数量、可发布资源数、`rsync`/`ssh` 是否在、SSH 能不能免密连通。
发现问题会以非零码退出。

---

## 目录与元数据

目录里每个 `*.md` / `*.markdown` 都会变成一篇文章，递归扫描，隐藏文件和
隐藏目录（`.git`、`.obsidian`）跳过。front matter 可选：

```markdown
---
title: 我的第一篇博客
slug: hello
date: 2026-01-05 10:30
tags: [Rust, 随笔]
summary: 一句话摘要
draft: false
---

正文从这里开始。
```

| 字段 | 没写时的回退 |
|------|--------------|
| `title` | 正文第一个 `# 标题`（并把它从正文去掉）；再没有就用文件名 |
| `slug` | 文件名，去掉 `YYYY-MM-DD-` 前缀 |
| `date` | 文件名前缀 `YYYY-MM-DD`；再没有就用文件修改时间 |
| `tags` | 无。支持 `[a, b]`、`a, b` 与 `- a` 换行列表；会去重、去 `#`、限长 |
| `summary` | 自动截取正文前 160 字 |
| `draft` | `false`。`draft: true` 不进首页、标签页和 RSS |

细节：

- 如果 front matter 的 `title` 与正文开头那个 `# 标题` **完全一样**，那个标题行
  也会被去掉，避免标题显示两遍；不一样则原样保留（那是正文内容）。
- 不带时区的 `date` 按**本机时区**理解。时间戳统一存 UTC（排序才正确），
  显示时转回本地时区。
- slug 保留中文：`我的第一篇博客.md` → `/posts/我的第一篇博客`。重名自动加 `-2`。

## 图片怎么放

**不需要改 Typora 的默认行为**，下面这些写法都能直接用：

| Typora 的写法 | 处理 |
|---------------|------|
| `![图](assets/a.png)` | 相对路径（默认「复制到 ./assets」）→ 重写为 `/media/assets/a.png` |
| `![图](my-post.assets/a.png)` | `${filename}.assets` 布局 → 相对**该 md 所在目录**解析 |
| `<img src="assets/a.png" style="zoom:50%;">` | 缩放图片时 Typora 会生成原始 HTML → 同样重写 |
| `![图](/Users/me/blog/assets/a.png)` | 绝对路径模式 → 只要在目录内就重写 |
| `![图](https://cdn.../a.png)` | 图床（PicGo）→ **原样保留** |
| `![图](data:image/png;base64,…)` | 内联 base64 → **原样保留** |

**原理**：文章页面在 `/posts/{slug}`，浏览器会拿 `assets/a.png` 去
`/posts/assets/a.png` 找，必然 404。所以渲染时把相对引用解析成绝对 URL
`/media/<相对内容目录的路径>`，服务端再映射回内容包的 `media/` 目录——
构建时资源就是按原路径镜像的，两边天然对得上。

**推荐的 Typora 设置**（偏好设置 → 图像）：插入图片时选「复制图片到 `./assets`」，
勾上「优先使用相对路径」。

**`/media` 的安全边界**（它映射的目录里同时放着你的 Markdown 源文件）：

- 只提供白名单扩展名（图片、音视频、pdf、zip），**`.md` 一律 404**
- 拒绝 `..` 与点开头的路径，并在 `canonicalize` 之后再校验是否仍在根目录内，
  符号链接也逃不出去
- SVG 带 `Content-Security-Policy: default-src 'none'; sandbox`，
  避免 SVG 里的脚本在站点同源下执行
- 带 ETag，支持 `If-None-Match` → 304

---

## 服务端

```bash
tblogs-server --data-dir /srv/tblogs/data --bind 127.0.0.1:8080
```

| 路由 | 说明 |
|------|------|
| `GET /` | 首页，`?page=N` 分页 |
| `GET /posts/{slug}` | 文章详情（草稿 404） |
| `GET /tags` / `GET /tags/{slug}` | 标签云 / 按标签筛选 |
| `GET /feed.xml` | RSS 2.0 |
| `GET /media/{*path}` | 图片等资源 |
| `GET /static/style.css` | 样式表（编译期内嵌） |
| `GET /healthz` | 健康检查，会真的查一次数据库 |

**它是只读的**：数据库用 `read_only` 打开，进程里没有任何写语句，也没有 `/admin`——
那些路由返回 404。systemd 单元里 `ProtectSystem=strict`，整个文件系统对它只读。

**站点设置随内容包走**。标题、描述、公开 URL 由 `build` 写进 `site.db` 的
`settings` 表，服务端启动时读出来，所以部署几乎不用配置。需要覆盖时用
环境变量：

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `TBLOGS_DATA_DIR` | `./data` | 存放 `site.db` 与 `media/` |
| `TBLOGS_BIND` | `127.0.0.1:8080` | 监听地址，默认只监听回环 |
| `TBLOGS_PAGE_SIZE` | `10` | 每页文章数 |
| `TBLOGS_RELOAD_INTERVAL` | `5` | 检查新内容包的间隔秒数，`0` 关闭 |
| `TBLOGS_SITE_TITLE` / `_SITE_DESCRIPTION` / `_SITE_URL` | 取自内容包 | 覆盖包里的值 |
| `RUST_LOG` | `tblogs=info,tower_http=warn` | 日志级别 |

**自动重载**：服务端轮询 `site.db` 的修改时间，发现变化就重新打开并原子换掉。
换成功后日志会打印 `reloaded the content package posts=N`；如果新文件还没传完，
打开失败就下一个周期重试（不会把失败当成已处理）。

---

## 部署与公网访问

`deploy/README.md` 详细说明了公网访问需要的条件，以及一个容易被忽略的事实：
**DDNS 只负责让域名跟上变化的 IP，它不做端口转发，也无法绕过运营商 NAT。**

一句话版本，要公网可达需要同时满足：

1. 你有一个**属于自己**的公网 IP（不是运营商 CGNAT 之后的共享出口）；
2. 你能在**持有该 IP 的设备上**配置端口转发；
3. 运营商没有封禁入站 80/443。

`bundle` 生成的 `check-exposure.sh` 会分别测 IPv4 与 IPv6 并给出判断；
`cloudflared-config.yml` 是绕开这一切的反向隧道方案（纯出站，无需公网 IP）。

装好之后：

```bash
# 服务器上
sudo bash /srv/tblogs/server/install.sh
sudo cp /srv/tblogs/server/Caddyfile /etc/caddy/Caddyfile   # 或选 nginx.conf
```

---

## 项目结构

```
src/
  bin/tblogs-server.rs   服务端入口
  bin/tblogs-admin.rs    管理端入口
  lib.rs                 共享库
  config.rs              服务端配置（clap + 环境变量）
  server.rs              只读服务、内容包加载与自动重载
  state.rs               当前内容包的快照（原子换）
  package.rs             构建内容包：写库 + 镜像资源
  bundle.rs              渲染可部署目录
  publish.rs             SSH 建目录 + rsync
  content.rs             扫描目录、指纹、同步成文章
  frontmatter.rs         front matter 解析与标题/日期回退
  assets.rs              相对引用 ↔ /media URL，以及安全反查
  markdown.rs            Markdown → HTML、摘要、图片 URL 重写
  db.rs                  schema、查询、只读/构建两种打开方式
  models.rs              Post / Tag / Settings 等
  slug.rs                生成 URL slug（保留 CJK）
  error.rs               统一错误页
  routes/                public / feed
templates/               askama 模板（编译期编入二进制）
static/style.css         样式，支持深色模式（编译期内嵌）
migrations/0001_schema.sql  唯一的 schema（库每次重建，无需迁移链）
scripts/smoke.sh         端到端测试
deploy/                  部署模板 + 公网接入说明（bundle 直接内嵌这些文件）
```

## 测试

```bash
cargo test                  # 80 个单元与集成测试
cargo clippy --all-targets
cargo fmt --check

./scripts/smoke.sh          # 68 项端到端检查
```

`scripts/smoke.sh` 自己搭 Typora 风格目录、构建内容包、起服务端，然后验证：
构建产物与只读可打开、全部路由、`/admin` 不存在、站点设置随包走、草稿、
资源服务与字节一致、ETag、目录穿越与源文件泄露防护、图片重写（相对/原始 HTML/
中文名/嵌套目录/远程不动）、**重新构建后自动重载**，以及 `bundle` 产出的每个文件
都已正确渲染、脚本语法有效。全是自包含的，跑完自己清理。

## 安全

- **服务器没有写路径。** 数据库只读打开，进程里没有写语句，也没有认证面。
- **`/media` 是白名单的**，源 `.md` 不可下载，穿越与符号链接逃逸都被拒绝。
- **`TBLOGS_BIND` 默认只监听回环**，要对外得显式改，或者（更好）交给反向代理。
- **`TBLOGS_SITE_URL` 要用 `https://`**，RSS 链接是绝对的。
- 上公网前至少：给反向代理配上 HTTPS、给 `/admin` 之类**已经不存在**的路径做限速
  就没必要了，但**给登录类接口限速**这里不适用——服务器上没有登录。
- 备份：内容包（`dist/`）和你的 Markdown 目录。服务器的 `data/` 是它们的副本，
  丢了重新 `publish` 一次即可。

## 许可

MIT
