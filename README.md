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

## 安装

```bash
cargo install --path . --locked     # 装到 ~/.cargo/bin，两个命令都可用
```

装完 `tblogs-admin` / `tblogs-server` 就在 PATH 上，**可以在任意目录调用**。
卸载用 `cargo uninstall tblogs`。

客户端是**单文件**的：schema 编译在二进制里，运行时不读任何外部文件。

## 日常使用

### 一次性配置

写进 `~/.zshrc`，之后命令就不用带参数了（每个选项都有对应的环境变量）：

```bash
export TBLOGS_CONTENT_DIR=~/nuts/markdown/blog    # 你的 Markdown 目录
export TBLOGS_DIST=~/tblogs-dist                  # 构建产物放哪
export TBLOGS_SITE_URL=https://blog.example.com   # 绝对地址，RSS 用
export TBLOGS_SITE_TITLE=我的技术笔记
export TBLOGS_DOMAIN=blog.example.com             # bundle 渲染配置用
export TBLOGS_TARGET=user@server:/srv/tblogs      # publish 的目标
```

> 构建产物**不要**放在内容目录里，否则每次构建都会同步进坚果云。它是可再生的，
> 放在内容目录外面就好。

### 写作

```bash
tblogs-admin preview
```

打开 <http://127.0.0.1:8080>，用 Typora 写，**保存即生效**：客户端监听目录并重建，
服务端发现新内容包后自己重载，两边都不需要重启。

### 发布

```bash
tblogs-admin build && tblogs-admin bundle && tblogs-admin publish
```

- `build` — 目录 → `site.db` + `media/`
- `bundle` — 再加上服务端二进制、systemd 单元、nginx/Caddy、DDNS 脚本
- `publish` — 一次 rsync 推上去；服务端约 5 秒内自动重载

发布前可以先 `tblogs-admin doctor` 自检（目录、依赖工具、SSH 连通性）。

> 如果你打算用静态托管（Cloudflare Pages 等），跳过 `bundle` 和 `publish`，
> 直接用 `export`——见下面的「静态部署」。

### 首次部署（只做一次）

```bash
# 在服务器上装一次，之后每次发布都不用再碰服务器
ssh user@server 'sudo bash /srv/tblogs/server/install.sh'

# 反向代理 + HTTPS，二选一
ssh user@server 'sudo cp /srv/tblogs/server/Caddyfile /etc/caddy/Caddyfile'   # 自动申请证书
ssh user@server 'sudo cp /srv/tblogs/server/nginx.conf /etc/nginx/sites-available/tblogs'
ssh user@server 'sudo certbot --nginx -d blog.example.com'

# 判断这台机器能不能被公网访问（重点看 IPv6）
ssh user@server 'sudo bash /srv/tblogs/server/check-exposure.sh'
```

网络、DDNS 与隧道的详细说明见 [`deploy/README.md`](deploy/README.md)。

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

### `export` — 渲染成静态站点

```bash
tblogs-admin export --out dist                # 写入 dist/site/
tblogs-admin export --out dist --page-size 20 # 每页文章数
```

把内容包渲染成一棵静态文件树，交给任意静态托管，不需要服务器。细节见
上面的「[静态部署](#静态部署不需要服务器)」。

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

## 站点图标

图标是符号 **`#`** —— Markdown 的标题符号,而这个博客正是 Markdown 驱动的。
底色用站点强调色 `--accent`。

选 `#` 的办法是**把候选渲染到真实的 16px 再挑**,而不是看大图猜。结果是:

| 候选 | 16px 下的表现 |
|---|---|
| **`#`** | ✅ 笔画粗、结构清楚 |
| `>` `=` `/` | ✅ 清楚但太普通 |
| `[ ]` `>_` | ⚠️ 偏细 |
| `</>` `{ }` | ❌ 三个字符挤成一团 |
| `&` `@` `$` `%` | ❌ 糊成一块 |

四种格式一起提供,全部编译进二进制,静态导出时也会写成真实文件:

| 文件 | 用途 |
|---|---|
| `favicon.svg` | 主图标,矢量,任意尺寸都锐利（不到 1 KB） |
| `favicon.ico` | 内嵌 16/32/48,浏览器默认会自己来取 |
| `favicon-32.png` | 不支持 SVG 图标时的回退 |
| `apple-touch-icon.png` | iOS 添加到主屏,180px |

换成别的符号或配色:

```bash
python3 -m venv /tmp/iconvenv && /tmp/iconvenv/bin/pip install fonttools pillow
# 编辑 scripts/make-icon.py 里的 GLYPH，比如 ">" 或 "_"
/tmp/iconvenv/bin/python scripts/make-icon.py
```

两点实现上的讲究:

- **SVG 里是路径而不是 `<text>`。** 依赖字体的图标在没有该字体的机器上会渲染成
  别的样子甚至空白,而图标的意义恰恰在于处处一致。
- **光栅版按字形「墨迹」缩放,而不是字号。** 墨迹总比字号小,按字号渲染会让 PNG
  比 SVG 小一圈,两种格式就对不上了。

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

## 静态部署（不需要服务器）

服务端是**只读**的：每个页面都是内容包的纯函数。所以整站可以预渲染成静态文件，
**功能上不丢任何东西**——不需要服务器、公网 IP、端口转发或 DDNS。

```bash
tblogs-admin build     # 先有内容包
tblogs-admin export    # 渲染成静态站点，写入 <dist>/site/
npx wrangler pages deploy dist/site    # 或交给任意静态托管
```

产物结构：

```
dist/site/
  index.html                              # 首页
  page/2.html  page/3.html                # 分页：查询串换成路径
  posts/<slug>.html                       # 每篇文章
  tags.html                               # 标签云
  tags/<slug>.html                        # 每个标签
  tags/<slug>/page/2.html                 # 标签的第二页起
  feed.xml  404.html   static/style.css   media/...
```

有三点值得注意：

- **分页从 `?page=2` 变成 `page/2`。** 静态托管无法路由查询串，所以页码进入路径，
  落盘为 `page/2.html`；托管会把它对应到无扩展名的 `/page/2`。
- **`404.html` 是必需的，不是装饰。** Cloudflare Pages 在没有它的项目里会当成
  单页应用，用首页回应**所有**未知路径，而不是返回 404。
- **页面里的链接与动态版完全一致**（都不带扩展名），所以部署到静态托管不必改
  URL 结构。

本地核对：`tblogs-admin preview` 的 URL 与静态版相同；要按静态方式核对，
用 `npx wrangler pages dev dist/site`。

> 静态托管要求站点位于**域名根目录**：模板里的链接是 `/posts/...` 这样的绝对
> 路径，放到子路径下会失效。

`export` 每次都会**清空**目标目录后重建。这是刻意的：改过名或取消发布的文章
否则会留下旧页面，继续被访问、继续被搜索引擎收录。

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
  pages.rs               构建页面（服务端与静态导出共用）
  export.rs              整站渲染成静态文件
  publish.rs             SSH 建目录 + rsync
  content.rs             扫描目录、指纹、同步成文章
  frontmatter.rs         front matter 解析与标题/日期回退
  assets.rs              相对引用 ↔ /media URL，以及安全反查
  markdown.rs            Markdown → HTML、摘要、图片 URL 重写
  db.rs                  schema、查询、只读/构建两种打开方式
  models.rs              Post / Tag / Settings 等
  slug.rs                生成 URL slug（保留 CJK）
  error.rs               统一错误页
  routes/                public / feed（只负责取页面并转成响应）
templates/               askama 模板（编译期编入二进制）
static/style.css         样式，支持深色模式（编译期内嵌）
static/favicon.svg       站点图标，`#` 符号轮廓（编译期内嵌）
static/favicon.ico       同一图标的 16/32/48 光栅版
static/favicon-32.png    SVG 不受支持时的回退
static/apple-touch-icon.png  iOS 主屏图标
scripts/make-icon.py     重新生成图标（改 GLYPH 即可换符号）
migrations/0001_schema.sql  唯一的 schema，编译期编入二进制（库每次重建，无需迁移链）
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
