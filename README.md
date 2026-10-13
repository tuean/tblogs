# tblogs

一个用 Rust 写的轻量博客系统：**axum** 提供 HTTP 服务、**SQLite** 存储内容、
**askama** 做服务端渲染、**pulldown-cmark** 渲染 Markdown。

没有前端构建步骤，没有 Node 依赖，没有 CDN。一个二进制 + 一个 `.db` 文件就是全部。

## 快速开始

```bash
cargo run
```

打开 <http://127.0.0.1:8080>。首次运行会用 `TBLOGS_SEED_DEMO=1` 插入三篇示例文章：

```bash
TBLOGS_SEED_DEMO=1 cargo run
```

想立刻带上管理口令（推荐，即使在本机）：

```bash
TBLOGS_ADMIN_TOKEN=$(openssl rand -hex 16) TBLOGS_SEED_DEMO=1 cargo run
```

写第一篇文章：打开 <http://127.0.0.1:8080/admin>。

## 环境变量

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `TBLOGS_BIND` | `127.0.0.1:8080` | HTTP 监听地址 |
| `TBLOGS_DATABASE_URL` | `sqlite://data/tblogs.db` | SQLite 路径，父目录会自动创建 |
| `TBLOGS_SITE_TITLE` | `我的博客` | 站点标题 |
| `TBLOGS_SITE_DESCRIPTION` | `用 Rust 构建的博客` | 站点描述（HTML meta 与 RSS） |
| `TBLOGS_SITE_URL` | `http://localhost:8080` | **绝对**基地址；RSS 里的链接必须是绝对 URL |
| `TBLOGS_ADMIN_TOKEN` | *未设置* | 设置后 `/admin` 需要令牌；未设置则后台不鉴权 |
| `TBLOGS_PAGE_SIZE` | `10` | 列表每页文章数 |
| `TBLOGS_SEED_DEMO` | *关闭* | 为真且数据库为空时插入示例文章 |
| `TBLOGS_CONTENT_DIR` | *未设置* | **本地 Markdown 目录**；设置后进入文件模式，见下一节 |
| `TBLOGS_SYNC_INTERVAL` | `5` | 轮询目录变化的间隔秒数；`0` 表示只在启动时同步一次 |
| `TBLOGS_MIGRATIONS_DIR` | 自动探测 | SQL 迁移目录；依次查找该变量、`./migrations`、可执行文件同级 `migrations/` |
| `RUST_LOG` | `tblogs=info,tower_http=warn` | 日志级别 |

`TBLOGS_BIND` **默认只监听回环地址**。这是刻意的：`cargo run` 不应该在你不
知情的情况下把服务暴露给所在网络。确实要对外提供时再显式改成 `0.0.0.0:8080`。

## 用本地目录写作（Typora 工作流）

设置 `TBLOGS_CONTENT_DIR` 后，**目录里的 Markdown 文件成为文章的唯一真相**：
改文件就是改文章，删文件就是删文章，不需要在网页端做任何操作。这样你可以完全
用 Typora（或任何编辑器）写博客。

```bash
mkdir -p ~/Documents/blog      # 目录必须先存在，否则启动即报错（故意的，防手滑）
TBLOGS_CONTENT_DIR=~/Documents/blog \
TBLOGS_SYNC_INTERVAL=2 \
cargo run
```

打开 <http://127.0.0.1:8080>。保存文件后**最多等一个轮询间隔**，页面就更新了，
不需要重启。

### 目录约定

- 递归扫描 `*.md` 和 `*.markdown`，每个文件一篇
- 隐藏文件和隐藏目录会被跳过（`.git`、`.obsidian`、`.typora` 等不会变成文章）
- 没有 front matter 也能用
- **文件型文章在网页端是只读的**。点编辑会返回 409 并提示去编辑器改——这是刻意的：
  否则网页上的改动会被下一次同步覆盖，删掉的文章还会自己回来
- 网页端创建的文章（没有对应文件）不受同步影响，可以照常编辑和删除

### 元数据：front matter 与回退规则

front matter 可选，用 `---` 包起来。支持这些键：

```markdown
---
title: 我的第一篇博客
slug: hello
date: 2026-01-05 10:30
tags: [Rust, 随笔]
summary: 一句话摘要
draft: false
---
```

| 字段 | 没写时的回退 |
|------|--------------|
| `title` | 正文第一个 `# 标题`（并把它从正文里去掉，避免重复显示）；再没有就用文件名 |
| `slug` | 文件名，去掉 `YYYY-MM-DD-` 前缀 |
| `date` | 文件名前缀 `YYYY-MM-DD-`；再没有就用文件修改时间 |
| `tags` | 无标签。支持 `[a, b]`、`a, b` 和 `- a` 换行列表 |
| `summary` | 自动截取正文前 160 字 |
| `draft` | `false`（即默认发布）。`draft: true` 只出现在管理后台 |

`tags` 里的半角逗号、全角逗号都能用。不带时区的 `date` 按**本机时区**理解。

### 图片怎么放（重点）

Typora 会按自己的设置写图片引用，本项目**不需要你改 Typora 的默认行为**，
下面这些形式都能直接用：

| Typora 的写法 | 处理方式 |
|---------------|----------|
| `![图](assets/a.png)` | 相对路径（Typora 默认「复制到 ./assets」）→ 重写为 `/media/assets/a.png` |
| `![图](my-post.assets/a.png)` | `${filename}.assets` 布局 → 相对**该 md 所在目录**解析 |
| `<img src="assets/a.png" style="zoom:50%;">` | Typora 缩放图片时会生成原始 HTML → 同样重写 `src` |
| `![图](/Users/me/blog/assets/a.png)` | 绝对路径（Typora「绝对路径」模式）→ 只要在内容目录内就重写 |
| `![图](https://cdn.example.com/a.png)` | 远程 URL（PicGo 图床）→ **原样保留** |
| `![图](data:image/png;base64,…)` | 内联 base64 → **原样保留** |

原理：渲染时把**相对于该文件**的引用解析成绝对 URL `/media/<相对内容目录的路径>`，
服务端再把 `/media/...` 映射回内容目录。因为文章页面在 `/posts/{slug}`，不重写的话
浏览器会去 `/posts/assets/a.png` 找图片，必然 404。

**推荐的 Typora 设置**（偏好设置 → 图像）：

- 插入图片时：**复制图片到 ./assets**（或 `./${filename}.assets`）
- 勾选 **优先使用相对路径**

这两项是 Typora 默认附近的选项，选上之后上面的机制完全透明。中文文件名、空格都
会正确百分号编码，实测可用。

**关于 `/media` 的安全性**，因为映射的目录里同时放着 Markdown 源文件：

- 只提供白名单扩展名（图片、音视频、pdf、zip），**`.md` 一律 404**，源文件不会泄露
- 拒绝 `..`、点开头文件，并对路径做 `canonicalize` 后再校验是否仍在根目录内，
  所以符号链接也逃不出去
- SVG 会带 `Content-Security-Policy: default-src 'none'; sandbox`，避免 SVG 里的脚本
  在站点同源下执行
- 带 ETag，浏览器可发 `If-None-Match` 拿 304，改了图片不用等缓存过期

## 部署：自包含的二进制

`templates/` 由 askama 在**编译期**编译进二进制，`static/style.css` 用
`include_str!` 内嵌，所以运行时**不需要**这两个目录。发布只需要两样东西：

```
/opt/tblogs/
  tbblogs                        # cargo build --release 的产物
  migrations/20260101000001_init.sql
```

已实测：把这两个东西复制到空目录直接运行，全部 55 项接口检查通过。

```bash
cargo build --release
install -D target/release/tblogs /opt/tblogs/tblogs
cp -r migrations /opt/tblogs/
```

`deploy/tblogs.service` 里 `WorkingDirectory=/opt/tblogs`，迁移目录会被自动找到。

如果使用**本地目录模式**，内容目录也要一起放到服务器上，并把
`TBLOGS_CONTENT_DIR` 指向它（`deploy/tblogs.service` 里有对应的注释行）。
同步是只读的：程序只会读目录，不会改写你的 Markdown 文件。

## 路由

公开页面：

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/` | 首页，已发布文章，`?page=N` 分页 |
| GET | `/posts/{slug}` | 文章详情（草稿返回 404） |
| GET | `/tags` | 标签云，带文章数 |
| GET | `/tags/{slug}` | 某标签下的文章 |
| GET | `/feed.xml` | RSS 2.0 |
| GET | `/healthz` | 健康检查，会真实查询数据库 |
| GET | `/static/style.css` | 样式表（编译期内嵌在二进制里） |
| GET | `/media/{*path}` | 内容目录里的图片等资源；未配置内容目录时 404 |

管理后台（设置 `TBLOGS_ADMIN_TOKEN` 后需登录）：

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/admin` | 文章列表（含草稿），标注来源是文件还是网页 |
| GET / POST | `/admin/login` | 登录表单 / 提交令牌 |
| POST | `/admin/logout` | 退出 |
| GET / POST | `/admin/new` | 新建表单 / 创建 |
| GET / POST | `/admin/posts/{id}/edit` | 编辑表单 / 保存（文件型文章返回 409） |
| POST | `/admin/posts/{id}/publish` | 在草稿与发布之间切换（文件型文章返回 409） |
| POST | `/admin/posts/{id}/delete` | 删除（文件型文章返回 409） |
| POST | `/admin/sync` | 立刻重新同步一次内容目录 |

所有修改都是 `POST`——HTML 表单不支持 `PUT`/`DELETE`。

## 写文章

正文是 Markdown，支持表格、代码块（带语言标注）、引用、任务列表、删除线、
脚注与标题属性。

- **Slug** 留空则由标题生成。中文标题会生成中文 slug（`我的第一篇博客` →
  `/posts/我的第一篇博客`），浏览器会做百分号编码，路由层再解码回来。
  标题相同会自动追加 `-2`、`-3`。
- **摘要** 留空则自动截取正文前 160 字（会先剥掉 Markdown 语法）。
- **标签** 用逗号分隔，半角 `,` 与全角 `，` 都可以，也接受换行或空格；
  标签名两两相同会去重，文章全部删除后孤立的标签会被清理。
- **状态** 草稿不会出现在首页、标签页、RSS 里，但一直能在 `/admin` 看到。

文件模式下的元数据规则见上面的「用本地目录写作」。

## 项目结构

```
src/
  main.rs        启动：日志、配置、数据库、首次同步、目录监听、优雅关闭
  config.rs      环境变量解析
  db.rs          SQLite：迁移、查询、写入、文件型文章的 upsert
  models.rs      Post / Tag / SourcedPost 及各类视图结构
  markdown.rs    Markdown → HTML、纯文本摘要、图片 URL 重写
  assets.rs      内容目录：相对引用 → /media URL，以及安全的反查
  content.rs     扫描目录、指纹比对、把文件同步成文章
  frontmatter.rs front matter 解析与标题/日期回退规则
  slug.rs        生成 URL slug（保留 CJK 字符）
  auth.rs        管理令牌校验与 Cookie
  error.rs       统一的错误页面
  state.rs       共享的 AppState
  templates.rs   askama 模板绑定
  routes/        public / admin / feed 三组路由
templates/       HTML 模板（编译期编入二进制）
static/style.css 样式，支持深色模式（编译期内嵌）
migrations/      SQL 迁移，启动时自动执行
scripts/smoke.sh          端到端冒烟测试（网页写作模式）
scripts/smoke-content.sh  端到端测试（本地目录 / 图片处理）
deploy/          部署与公网接入（含 DDNS）说明
```

## 测试

```bash
cargo test              # 67 个单元与数据库集成测试
cargo clippy --all-targets
cargo fmt --check
```

`scripts/smoke.sh` 是对运行中实例的端到端测试，覆盖全部路由以及
「新建 → 发布 → 转草稿 → 重新发布 → 删除」完整生命周期：

```bash
cargo run &                       # 或另开一个终端
./scripts/smoke.sh

# 验证带口令的鉴权分支
BASE=http://127.0.0.1:8099 ADMIN_TOKEN=secret ./scripts/smoke.sh
```

`scripts/smoke-content.sh` 覆盖本地目录模式：它会自己搭一个 Typora 风格的
目录（含真实 PNG、中文文件名、子目录、原始 HTML `<img>`、草稿、目录穿越
攻击等），然后验证导入、图片重写、`/media` 服务与安全边界、只读规则、
热更新和删除：

```bash
mkdir -p /tmp/tblogs-smoke-content
rm -f data/smoke.db
TBLOGS_DATABASE_URL=sqlite://data/smoke.db \
TBLOGS_CONTENT_DIR=/tmp/tblogs-smoke-content \
TBLOGS_SYNC_INTERVAL=2 cargo run &

TBLOGS_DB=data/smoke.db ./scripts/smoke-content.sh
```

## 关于公网访问

**`deploy/README.md` 详细说明了把服务映射到公网需要满足的条件**，以及为什么
「DDNS + 端口映射」在多数办公网络里走不通。

一句话版本：DDNS 只负责让域名跟上会变化的 IP，它**不负责**端口转发，也无法
绕过运营商 NAT。要真正对外提供服务，需要满足：

1. 你拥有一个公网 IP（而不是运营商 NAT 之后的共享出口）；
2. 你能在**拥有该 IP 的设备**上配置端口转发；
3. 运营商没有封禁 80/443 入站。

如果这些条件不成立，`deploy/` 里给了两条可行路线：部署到云主机，或者用反向
隧道（Cloudflare Tunnel / frp）从内网主动外连。

## 上公网前必须处理的安全项

当前实现是**为本机使用**设计的。在把端口暴露到公网之前，至少要做到：

- [ ] 设置 `TBLOGS_ADMIN_TOKEN`（否则任何人可写文章）
- [ ] 用 `TBLOGS_SITE_URL=https://…` 让会话 Cookie 自动带上 `Secure`
- [ ] 在反向代理上启用 HTTPS（见 `deploy/nginx.conf`）
- [ ] 加登录失败限速——目前没有防暴力破解
- [ ] 考虑把 `/admin` 限制到内网或 VPN，而不是直接暴露
- [ ] 备份 `data/tblogs.db`（连同 `-wal` 文件，或在停机后复制）
- [ ] 使用目录模式时，`TBLOGS_CONTENT_DIR` 里的**草稿文件也会出现在服务器磁盘上**；
      如果那台机器不是你自己的，先想清楚这一点

现有的令牌校验用固定时间比较，避免时序侧信道；但它只是一个共享口令，
不是完整的账号体系。

## 许可

MIT
