//! Generating a deployable server directory.
//!
//! `tblogs-admin bundle` turns a content package into everything needed to run
//! it on a remote machine: the server binary, a rendered systemd unit, an nginx
//! or Caddy configuration, the Cloudflare Tunnel and DDNS helpers, and an
//! install script that ties them together.
//!
//! The configuration templates are **embedded in this binary** rather than read
//! from a checkout, so `bundle` works from anywhere. They are rendered with the
//! operator's real domain and paths instead of placeholders left to hand-edit.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::MEDIA_DIR;

const SYSTEMD_UNIT: &str = include_str!("../deploy/tblogs.service");
const NGINX_CONFIG: &str = include_str!("../deploy/nginx.conf");
const CADDY_CONFIG: &str = include_str!("../deploy/Caddyfile");
const CLOUDFLARED_CONFIG: &str = include_str!("../deploy/cloudflared-config.yml");
const DDNS_SCRIPT: &str = include_str!("../deploy/ddns-update.sh");
const EXPOSURE_SCRIPT: &str = include_str!("../deploy/check-exposure.sh");

/// Placeholder hostname used throughout the deployment templates.
const PLACEHOLDER_DOMAIN: &str = "blog.example.com";
/// Placeholder install root used throughout the deployment templates.
const PLACEHOLDER_ROOT: &str = "/opt/tblogs";

/// Options for one bundle.
#[derive(Debug, Clone)]
pub struct BundleOptions {
    /// Public hostname the site will be served from, e.g. `blog.example.com`.
    pub domain: String,
    /// Where the server is installed on the remote machine.
    pub install_dir: PathBuf,
    /// Directory holding `site.db` and `media/` on the remote machine.
    pub data_dir: PathBuf,
    /// Address the server binds to on the remote machine.
    pub bind: String,
    /// Copy this in as `tblogs-server`. When `None`, `bundle` looks for a
    /// `tblogs-server` beside the running executable.
    pub server_binary: Option<PathBuf>,
}

/// What `bundle` produced.
#[derive(Debug, Clone)]
pub struct BundleReport {
    pub directory: PathBuf,
    pub files: Vec<String>,
    /// The binary that was copied in, if one was found.
    pub server_binary: Option<PathBuf>,
}

/// Render the deployment directory at `<out>/server`.
///
/// `out` is the same directory the content package was built into, so the whole
/// thing publishes in a single `rsync`.
pub fn write_bundle(out: &Path, options: &BundleOptions) -> Result<BundleReport> {
    let directory = out.join("server");
    std::fs::create_dir_all(&directory)
        .with_context(|| format!("could not create {}", directory.display()))?;

    let domain = options.domain.trim();
    if domain.is_empty() {
        anyhow::bail!("a domain is required to render the deployment files");
    }

    let root = options.install_dir.display().to_string();
    let data = options.data_dir.display().to_string();
    let bind = options.bind.trim();

    // One substitution pass per template. The templates are written with
    // realistic values so they still read correctly as standalone examples in
    // deploy/.
    let render = |template: &str| -> String {
        template
            .replace(PLACEHOLDER_DOMAIN, domain)
            .replace(PLACEHOLDER_ROOT, &root)
    };

    let mut files: Vec<String> = Vec::new();
    let mut write = |name: &str, contents: &str| -> Result<()> {
        let path = directory.join(name);
        std::fs::write(&path, contents)
            .with_context(|| format!("could not write {}", path.display()))?;
        files.push(name.to_string());
        Ok(())
    };

    // Order matters: the install-root substitution would rewrite the data
    // directory line first (the default data directory lives under the install
    // root), leaving nothing for the exact-match replacement to find.
    write(
        "tblogs.service",
        &render(&render_server_unit(SYSTEMD_UNIT, &data, bind)?),
    )?;
    write("nginx.conf", &render(NGINX_CONFIG))?;
    write("Caddyfile", &render(CADDY_CONFIG))?;
    write("cloudflared-config.yml", &render(CLOUDFLARED_CONFIG))?;
    write("ddns-update.sh", DDNS_SCRIPT)?;
    write("check-exposure.sh", EXPOSURE_SCRIPT)?;
    write("tblogs.env.example", ENV_EXAMPLE)?;
    write("README.md", &bundle_readme(domain, &root, &data, bind))?;
    write("install.sh", &install_script(domain, &root, &data))?;

    for script in ["ddns-update.sh", "check-exposure.sh", "install.sh"] {
        make_executable(&directory.join(script))?;
    }

    // A binary the operator named explicitly has to exist; failing to find one
    // by default is survivable, so that a bundle can still be produced and
    // inspected on a machine that has not built the server.
    if let Some(path) = &options.server_binary
        && !path.is_file()
    {
        anyhow::bail!("server binary not found: {}", path.display());
    }

    let server_binary = match resolve_server_binary(options.server_binary.as_deref()) {
        Ok(source) => {
            let destination = directory.join("tblogs-server");
            std::fs::copy(&source, &destination).with_context(|| {
                format!(
                    "could not copy {} to {}",
                    source.display(),
                    destination.display()
                )
            })?;
            // `std::fs::copy` does not reliably carry the executable bit.
            make_executable(&destination)?;
            files.push("tblogs-server".to_string());
            Some(source)
        }
        Err(error) => {
            tracing::warn!(%error, "no server binary was bundled");
            None
        }
    };

    Ok(BundleReport {
        directory,
        files,
        server_binary,
    })
}

/// Apply the data directory and bind address to the systemd unit.
///
/// Substitution is by exact string match against the shipped template, so it
/// is verified rather than assumed: a template edit that removes one of those
/// lines would otherwise produce a unit that silently ignores the operator's
/// choices. The check turns that drift into a loud failure.
fn render_server_unit(unit: &str, data_dir: &str, bind: &str) -> Result<String> {
    const DATA_DIR_LINE: &str = "Environment=TBLOGS_DATA_DIR=/opt/tblogs/data";
    const BIND_LINE: &str = "Environment=TBLOGS_BIND=127.0.0.1:8080";

    for line in [DATA_DIR_LINE, BIND_LINE] {
        if !unit.contains(line) {
            anyhow::bail!(
                "deploy/tblogs.service no longer contains {line:?}; \
                 update render_server_unit in src/bundle.rs to match the template"
            );
        }
    }

    Ok(unit
        .replace(
            DATA_DIR_LINE,
            &format!("Environment=TBLOGS_DATA_DIR={data_dir}"),
        )
        .replace(BIND_LINE, &format!("Environment=TBLOGS_BIND={bind}")))
}

const ENV_EXAMPLE: &str = "\
# Optional overrides for the tblogs server.
#
# The site title, description and public URL are baked into the content package
# by `tblogs-admin build`, so nothing here is required. Set these only to
# override what the package says.
#
# TBLOGS_SITE_TITLE=我的博客
# TBLOGS_SITE_URL=https://blog.example.com
# TBLOGS_RELOAD_INTERVAL=5
# RUST_LOG=tblogs=info,tower_http=warn
";

fn bundle_readme(domain: &str, root: &str, data_dir: &str, bind: &str) -> String {
    format!(
        r#"# tblogs server bundle

为 `{domain}` 生成的部署文件。编译好的服务端是**只读**的：它只提供 HTTP
服务，不写任何数据，也不接受内容修改。

## 内容

| 文件 | 用途 |
|------|------|
| `tblogs-server` | 服务端二进制（构建过才会有） |
| `install.sh` | 安装脚本，在服务器上以 root 运行 |
| `tblogs.service` | systemd 单元 |
| `nginx.conf` / `Caddyfile` | 反向代理与 HTTPS，二选一 |
| `ddns-update.sh` | Cloudflare DDNS 更新（A / AAAA） |
| `check-exposure.sh` | 诊断本机能否被公网访问 |
| `cloudflared-config.yml` | Cloudflare Tunnel 配置（无需公网 IP） |
| `tblogs.env.example` | 可选的环境变量覆盖 |

## 安装

内容包（`site.db` 与 `media/`）在上一级目录，安装脚本会把它们放到
`{data_dir}`。

```bash
sudo bash server/install.sh
```

脚本会创建用户与目录、安装二进制到 `{root}`、放置内容包、安装并启动
systemd 服务，然后在末尾打印后续步骤。可以重复运行：它会升级二进制与内容
并重启服务，但**不会**覆盖你的 nginx / Caddy 配置。

## 生成时使用的参数

- 域名：`{domain}`
- 安装目录：`{root}`
- 数据目录：`{data_dir}`
- 监听：`{bind}`（只监听回环，由反向代理对外）

## 接下来

1. **TLS**：从 `nginx.conf` 与 `Caddyfile` 里选一个。Caddy 会自动申请证书，
   配置更短；用 nginx 的话再跑 `certbot --nginx -d {domain}`。
2. **DNS**：让 `{domain}` 指向这台机器。IP 会变就用 `ddns-update.sh` 维护。
3. **判断网络**：跑 `check-exposure.sh`。如果这台机器没有公网 IP（运营商
   CGNAT）或者入站被封，改用 `cloudflared-config.yml` 走反向隧道。
4. **之后发布内容**：在本地跑 `tblogs-admin publish`。服务端会检测到新的
   `site.db` 并自动重新加载，不需要重启。

服务端的 `TBLOGS_SITE_URL` 决定 RSS 里的绝对链接。默认取自内容包里烘焙的值，
所以只要 build 时填对了就不用管。
"#
    )
}

fn install_script(domain: &str, root: &str, data_dir: &str) -> String {
    // `{{` / `}}` are the escapes this format string needs for the literal
    // braces that appear in the embedded shell script.
    format!(
        r#"#!/usr/bin/env bash
#
# Install the tblogs server. Run as root on the target machine, from the
# directory holding this script.
#
#   sudo bash install.sh
#
# Idempotent: re-running upgrades the binary and the content and restarts the
# service. It never touches the nginx or Caddy configuration, because that is
# where your own edits live.

set -euo pipefail

DOMAIN="{domain}"
ROOT="{root}"
DATA_DIR="{data_dir}"
SERVICE_USER="tblogs"
PORT="8080"

HERE="$(cd "$(dirname "${{BASH_SOURCE[0]}}")" && pwd)"
PACKAGE="$(dirname "$HERE")"

if [[ "$(id -u)" -ne 0 ]]; then
  echo "run this with sudo" >&2
  exit 1
fi

if [[ ! -f "$PACKAGE/site.db" ]]; then
  echo "error: $PACKAGE/site.db not found" >&2
  echo "       publish the content package alongside this script" >&2
  exit 1
fi

echo "==> installing to $ROOT"
install -d -m 0755 "$ROOT"

if [[ -f "$HERE/tblogs-server" ]]; then
  install -m 0755 "$HERE/tblogs-server" "$ROOT/tblogs-server"
  echo "    binary installed"
else
  echo "    no binary in the bundle; keeping the existing one"
  if [[ ! -x "$ROOT/tblogs-server" ]]; then
    echo "error: no server binary to install" >&2
    exit 1
  fi
fi

echo "==> service account and data directory"
if ! id -u "$SERVICE_USER" >/dev/null 2>&1; then
  useradd --system --no-create-home --shell /usr/sbin/nologin "$SERVICE_USER"
fi
install -d -m 0750 -o "$SERVICE_USER" -g "$SERVICE_USER" "$DATA_DIR"
install -d -m 0750 -o "$SERVICE_USER" -g "$SERVICE_USER" "$DATA_DIR/MEDIA_DIR_MARKER"

echo "==> publishing content"
# Stage beside the live database and rename into place, so a running server
# never observes a half-written file.
install -m 0640 -o "$SERVICE_USER" -g "$SERVICE_USER" \
  "$PACKAGE/site.db" "$DATA_DIR/site.db.new"
mv -f "$DATA_DIR/site.db.new" "$DATA_DIR/site.db"

if [[ -d "$PACKAGE/MEDIA_DIR_MARKER" ]]; then
  rm -rf "$DATA_DIR/MEDIA_DIR_MARKER.new"
  cp -a "$PACKAGE/MEDIA_DIR_MARKER" "$DATA_DIR/MEDIA_DIR_MARKER.new"
  rm -rf "$DATA_DIR/MEDIA_DIR_MARKER"
  mv "$DATA_DIR/MEDIA_DIR_MARKER.new" "$DATA_DIR/MEDIA_DIR_MARKER"
  chown -R "$SERVICE_USER:$SERVICE_USER" "$DATA_DIR/MEDIA_DIR_MARKER"
fi

echo "==> systemd unit"
install -m 0644 "$HERE/tblogs.service" /etc/systemd/system/tblogs.service
systemctl daemon-reload
systemctl enable tblogs.service >/dev/null
systemctl restart tblogs.service

sleep 1
if systemctl is-active --quiet tblogs.service; then
  echo "    tblogs.service is running"
else
  echo "    tblogs.service failed to start:" >&2
  journalctl -u tblogs.service -n 20 --no-pager >&2
  exit 1
fi

cat <<EOF

Installed. The server listens on 127.0.0.1:$PORT and is reachable only through
a reverse proxy.

Next steps:

  1. Check it locally:
       curl -s http://127.0.0.1:$PORT/healthz

  2. Put a reverse proxy in front of it, using either:
       $HERE/nginx.conf    (then: certbot --nginx -d $DOMAIN)
       $HERE/Caddyfile     (copy to /etc/caddy/Caddyfile)

  3. Point $DOMAIN at this machine and keep the record current with:
       $HERE/ddns-update.sh
     or expose it with a tunnel instead:
       $HERE/cloudflared-config.yml

  4. Find out whether this network allows inbound traffic at all:
       $HERE/check-exposure.sh

Afterwards publish new content from the authoring machine with
tblogs-admin publish. This server reloads the database automatically, so no
restart is needed.
EOF
"#
    )
    .replace("MEDIA_DIR_MARKER", MEDIA_DIR)
}

/// Find the server binary to bundle.
///
/// Prefers an explicit path, then a `tblogs-server` beside the running
/// executable, which is where `cargo build` leaves it.
fn resolve_server_binary(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        if !path.is_file() {
            anyhow::bail!("server binary not found: {}", path.display());
        }
        return Ok(path.to_path_buf());
    }

    let current = std::env::current_exe().context("could not locate the running executable")?;
    let sibling = current
        .parent()
        .map(|dir| dir.join("tblogs-server"))
        .context("the running executable has no parent directory")?;

    if sibling.is_file() {
        return Ok(sibling);
    }

    anyhow::bail!(
        "no tblogs-server beside {}; run `cargo build --release` first, or pass --server-binary",
        current.display()
    )
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = std::fs::metadata(path)
        .with_context(|| format!("could not stat {}", path.display()))?
        .permissions();

    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions)
        .with_context(|| format!("could not chmod {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> BundleOptions {
        BundleOptions {
            domain: "blog.test.example".to_string(),
            install_dir: PathBuf::from("/srv/tblogs"),
            data_dir: PathBuf::from("/srv/tblogs/data"),
            bind: "127.0.0.1:9000".to_string(),
            server_binary: None,
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tblogs-bundle-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn bundle_renders_the_domain_and_paths() {
        let out = scratch("render");
        let report = write_bundle(&out, &options()).unwrap();

        let nginx = std::fs::read_to_string(report.directory.join("nginx.conf")).unwrap();
        assert!(
            nginx.contains("blog.test.example"),
            "domain not substituted"
        );
        assert!(
            !nginx.contains(PLACEHOLDER_DOMAIN),
            "domain placeholder survived"
        );

        let unit = std::fs::read_to_string(report.directory.join("tblogs.service")).unwrap();
        assert!(
            unit.contains("TBLOGS_DATA_DIR=/srv/tblogs/data"),
            "got {unit}"
        );
        assert!(unit.contains("TBLOGS_BIND=127.0.0.1:9000"), "got {unit}");
        // The server no longer has a content directory of its own.
        assert!(!unit.contains("TBLOGS_CONTENT_DIR="), "stale directive");

        let install = std::fs::read_to_string(report.directory.join("install.sh")).unwrap();
        assert!(install.contains(r#"ROOT="/srv/tblogs""#));
        assert!(install.contains(r#"DATA_DIR="/srv/tblogs/data""#));
        assert!(
            !install.contains("MEDIA_DIR_MARKER"),
            "media marker survived"
        );
        assert!(install.contains("media.new"), "got {install}");

        // Nothing anywhere may still carry a placeholder.
        for entry in std::fs::read_dir(&report.directory).unwrap() {
            let entry = entry.unwrap();
            if !entry.file_type().unwrap().is_file() {
                continue;
            }
            let contents = std::fs::read_to_string(entry.path()).unwrap_or_default();
            assert!(
                !contents.contains(PLACEHOLDER_ROOT),
                "{} still contains {PLACEHOLDER_ROOT}",
                entry.path().display()
            );
        }

        std::fs::remove_dir_all(&out).unwrap();
    }

    #[test]
    fn a_missing_server_binary_is_not_fatal() {
        let out = scratch("nobin");

        // No binary available and none requested: the configuration is still
        // produced, so a bundle can be inspected without a full build.
        let report = write_bundle(&out, &options()).unwrap();
        assert!(report.files.contains(&"install.sh".to_string()));

        // An explicitly requested binary that does not exist is an error.
        let bad = BundleOptions {
            server_binary: Some(PathBuf::from("/definitely/not/here")),
            ..options()
        };
        assert!(write_bundle(&out, &bad).is_err());

        std::fs::remove_dir_all(&out).unwrap();
    }

    #[test]
    fn a_drifted_service_template_is_reported_not_ignored() {
        // If the shipped unit stops carrying the lines that `bundle`
        // substitutes, the result would silently ignore the operator's
        // --data-dir and --bind. That must be an error, not a surprise later.
        let drifted = SYSTEMD_UNIT.replace(
            "Environment=TBLOGS_DATA_DIR=/opt/tblogs/data",
            "Environment=TBLOGS_DATA_DIR=/somewhere/else",
        );
        assert!(render_server_unit(&drifted, "/srv/tblogs/data", "127.0.0.1:9000").is_err());

        // The real template still renders.
        let rendered =
            render_server_unit(SYSTEMD_UNIT, "/srv/tblogs/data", "127.0.0.1:9000").unwrap();
        assert!(rendered.contains("TBLOGS_DATA_DIR=/srv/tblogs/data"));
        assert!(rendered.contains("TBLOGS_BIND=127.0.0.1:9000"));
    }

    #[test]
    fn an_empty_domain_is_rejected() {
        let out = scratch("nodomain");
        let bad = BundleOptions {
            domain: "   ".to_string(),
            ..options()
        };
        assert!(write_bundle(&out, &bad).is_err());
        std::fs::remove_dir_all(&out).unwrap();
    }
}
