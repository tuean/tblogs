//! `tblogs-admin` — the local authoring client.
//!
//! Point it at a directory of Markdown files and it produces a self-contained
//! content package, generates everything needed to run that package on a
//! server, and publishes it over SSH. Nothing here ever runs on the server.

use std::{net::SocketAddr, path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use tblogs::{
    assets::Assets,
    bundle::{self, BundleOptions},
    config::{self, ServerConfig},
    content, db, export,
    models::Settings,
    package,
    publish::{self, Target},
    server,
};

#[derive(Debug, Parser)]
#[command(
    name = "tblogs-admin",
    about = "Author a tblogs site from a local Markdown directory",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Action,
}

#[derive(Debug, Subcommand)]
enum Action {
    /// Build a content package from a directory of Markdown files.
    Build(BuildArgs),

    /// Build, then serve the package locally so you can look at it.
    Preview(PreviewArgs),

    /// Add the deployable server files to a package.
    Bundle(BundleArgs),

    /// Render the package to static files, for hosting without a server.
    Export(ExportArgs),

    /// Copy a package to a server over SSH.
    Publish(PublishArgs),

    /// Check the local setup, and optionally the publish target.
    Doctor(DoctorArgs),
}

#[derive(Debug, Args, Clone)]
struct BuildArgs {
    /// Directory containing your Markdown files.
    #[arg(long, env = "TBLOGS_CONTENT_DIR", value_name = "DIR")]
    content_dir: PathBuf,

    /// Where to write the package.
    #[arg(long, env = "TBLOGS_DIST", default_value = DEFAULT_DIST_DIR_STR)]
    out: PathBuf,

    /// Public base URL. RSS links are absolute, so this has to be right.
    #[arg(long, env = "TBLOGS_SITE_URL", default_value = "http://localhost:8080")]
    site_url: String,

    /// Site title, shown in the header, the page title and the feed.
    #[arg(long, env = "TBLOGS_SITE_TITLE", default_value = "我的博客")]
    title: String,

    /// Site description, used for the meta description and the feed.
    #[arg(long, env = "TBLOGS_SITE_DESCRIPTION", default_value = "")]
    description: String,
}

const DEFAULT_DIST_DIR_STR: &str = "dist";

#[derive(Debug, Args)]
struct PreviewArgs {
    #[command(flatten)]
    build: BuildArgs,

    /// Address to serve the preview on.
    #[arg(long, default_value = "127.0.0.1:8080")]
    bind: SocketAddr,

    /// Do not rebuild when the content directory changes.
    #[arg(long)]
    no_watch: bool,

    /// How often to look for changes, in seconds.
    #[arg(long, default_value_t = 2)]
    watch_interval: u64,
}

#[derive(Debug, Args)]
struct BundleArgs {
    /// Package directory produced by `build`.
    #[arg(long, env = "TBLOGS_DIST", default_value = DEFAULT_DIST_DIR_STR)]
    out: PathBuf,

    /// Public hostname the site will be served from.
    #[arg(long, env = "TBLOGS_DOMAIN", value_name = "DOMAIN")]
    domain: String,

    /// Install root on the server.
    #[arg(long, default_value = "/opt/tblogs")]
    install_dir: PathBuf,

    /// Data directory on the server, holding `site.db` and `media/`.
    /// Defaults to `<install-dir>/data`.
    #[arg(long, value_name = "DIR")]
    data_dir: Option<PathBuf>,

    /// Address the server binds to on the remote machine.
    #[arg(long, default_value = "127.0.0.1:8080")]
    bind: String,

    /// Server binary to bundle. Defaults to `tblogs-server` beside this one.
    #[arg(long, value_name = "PATH")]
    server_binary: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct ExportArgs {
    /// Package directory produced by `build`.
    #[arg(long, env = "TBLOGS_DIST", default_value = DEFAULT_DIST_DIR_STR)]
    out: PathBuf,

    /// Where to write the static site. Defaults to `<out>/site`.
    #[arg(long, env = "TBLOGS_SITE_DIR", value_name = "DIR")]
    site_dir: Option<PathBuf>,

    /// Posts per listing page. Must match what `preview` uses, or the
    /// pagination will not line up with what you saw locally.
    #[arg(long, env = "TBLOGS_PAGE_SIZE", default_value_t = 10)]
    page_size: i64,
}

#[derive(Debug, Args)]
struct PublishArgs {
    /// Package directory to send.
    #[arg(long, env = "TBLOGS_DIST", default_value = DEFAULT_DIST_DIR_STR)]
    out: PathBuf,

    /// Destination, as `user@host:/path`.
    #[arg(long, env = "TBLOGS_TARGET", value_name = "USER@HOST:/PATH")]
    to: String,

    /// Show what would change without transferring anything.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct DoctorArgs {
    /// Content directory to check.
    #[arg(long, env = "TBLOGS_CONTENT_DIR", value_name = "DIR")]
    content_dir: Option<PathBuf>,

    /// Publish target to check reachability for.
    #[arg(long, env = "TBLOGS_TARGET", value_name = "USER@HOST:/PATH")]
    to: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // The library logs through `tracing`; in a CLI that should be quiet unless
    // something is wrong, so only warnings and errors surface by default.
    init_logging();

    let cli = Cli::parse();

    let outcome = match cli.command {
        Action::Build(args) => build(&args).await,
        Action::Preview(args) => preview(args).await,
        Action::Bundle(args) => bundle(&args),
        Action::Export(args) => export_site(&args).await,
        Action::Publish(args) => publish(&args),
        Action::Doctor(args) => doctor(&args),
    };

    if let Err(error) = outcome {
        eprintln!("\n错误: {error:#}");
        std::process::exit(1);
    }

    Ok(())
}

fn init_logging() {
    use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));

    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_target(false))
        .try_init();
}

// ------------------------------------------------------------------ commands

async fn build(args: &BuildArgs) -> Result<()> {
    let report = build_package(args).await?;

    println!();
    println!("内容包已生成: {}", args.out.display());
    println!("  {}", report.summary());
    println!();
    println!(
        "本地预览:   tblogs-admin preview --content-dir {}",
        args.content_dir.display()
    );
    println!("生成部署包: tblogs-admin bundle --domain <你的域名>");
    println!(
        "发布:       tblogs-admin publish --to user@host:{}",
        args.out.display()
    );

    Ok(())
}

async fn build_package(args: &BuildArgs) -> Result<package::BuildReport> {
    let settings = Settings {
        site_title: args.title.clone(),
        site_description: args.description.clone(),
        site_url: args.site_url.trim_end_matches('/').to_string(),
        built_at: db::now_iso(),
    };

    println!("正在扫描 {}", args.content_dir.display());

    let log = |message: &str| println!("  {message}");

    package::build_package(&args.content_dir, &args.out, &settings, &log).await
}

async fn preview(args: PreviewArgs) -> Result<()> {
    let report = build_package(&args.build).await?;

    let config = ServerConfig {
        bind: args.bind,
        data_dir: args.build.out.clone(),
        page_size: 10,
        // A short interval so a rebuild is picked up promptly. The server
        // reloads the package by itself when the database changes.
        reload_interval: Duration::from_secs(1),
        site_title: None,
        site_description: None,
        site_url: None,
    };

    println!();
    println!("预览: http://{}", config.bind);
    println!("  {}", report.summary());

    if args.no_watch {
        println!("  (未开启自动重建)");
    } else {
        println!(
            "  监听 {} 的变化，改动会自动重建",
            args.build.content_dir.display()
        );

        let build_args = args.build.clone();
        let interval = Duration::from_secs(args.watch_interval.max(1));
        tokio::spawn(async move {
            watch_and_rebuild(build_args, interval).await;
        });
    }

    println!();
    server::run(config).await
}

/// Rebuild the package whenever the content directory changes.
///
/// The running server notices the new database and reloads it, so a saved file
/// shows up in the browser without restarting anything.
async fn watch_and_rebuild(args: BuildArgs, interval: Duration) {
    let fingerprint = |dir: &PathBuf| -> Option<u64> {
        content::scan(dir)
            .ok()
            .map(|files| content::fingerprint(&files).value())
    };

    let mut last = fingerprint(&args.content_dir);

    loop {
        tokio::time::sleep(interval).await;

        let current = fingerprint(&args.content_dir);
        if current.is_none() || current == last {
            continue;
        }

        match build_package(&args).await {
            Ok(report) => {
                last = current;
                println!("已重建: {}", report.summary());
            }
            Err(error) => {
                // Keep the previous package on screen and try again next tick.
                eprintln!("重建失败: {error:#}");
            }
        }
    }
}

fn bundle(args: &BundleArgs) -> Result<()> {
    // Deriving the default from the install root keeps the two in step when
    // `--install-dir` is moved, which is the common case.
    let data_dir = args
        .data_dir
        .clone()
        .unwrap_or_else(|| args.install_dir.join("data"));

    let options = BundleOptions {
        domain: args.domain.clone(),
        install_dir: args.install_dir.clone(),
        data_dir,
        bind: args.bind.clone(),
        server_binary: args.server_binary.clone(),
    };

    let report = bundle::write_bundle(&args.out, &options)?;

    println!();
    println!("部署文件已生成: {}", report.directory.display());
    for file in &report.files {
        println!("  {file}");
    }

    match &report.server_binary {
        Some(path) => println!("\n服务端二进制来自 {}", path.display()),
        None => println!(
            "\n注意: 没有找到 tblogs-server，安装脚本需要它。\n\
             先运行 `cargo build --release`，或用 --server-binary 指定路径。"
        ),
    }

    println!();
    println!("发布整个目录: tblogs-admin publish --to user@host:<远端目录>");
    println!(
        "然后在服务器上: sudo bash {}/install.sh",
        report.directory.display()
    );

    Ok(())
}

async fn export_site(args: &ExportArgs) -> Result<()> {
    let site_dir = args
        .site_dir
        .clone()
        .unwrap_or_else(|| args.out.join("site"));

    let database = args.out.join(config::DATABASE_FILE);
    let media = args.out.join(config::MEDIA_DIR);

    if !database.is_file() {
        bail!(
            "{} 不存在。先运行 `tblogs-admin build`。",
            database.display()
        );
    }

    let pool = db::open_read_only(&database)
        .await
        .with_context(|| format!("打开 {} 失败", database.display()))?;

    let settings = db::load_settings(&pool).await?;
    let assets = Assets::new(&media)?;

    println!("正在导出静态站点");
    let mut log = |message: &str| println!("{message}");

    let report = export::export_site(
        &pool,
        &assets,
        &settings,
        args.page_size,
        &site_dir,
        &media,
        &mut log,
    )
    .await?;

    pool.close().await;

    println!();
    println!("静态站点已生成: {}", site_dir.display());
    println!(
        "  {} 个 HTML, {} 个媒体文件, {:.1} MiB",
        report.html_files,
        report.media_files,
        report.bytes as f64 / (1024.0 * 1024.0)
    );
    println!();
    println!("本地核对:   tblogs-admin preview        （URL 与静态版一致）");
    println!(
        "按静态方式核对: npx wrangler pages dev {}",
        site_dir.display()
    );
    println!(
        "部署到 Cloudflare Pages: npx wrangler pages deploy {}",
        site_dir.display()
    );
    println!("或把整个目录交给任意静态托管 / nginx。");

    Ok(())
}

fn publish(args: &PublishArgs) -> Result<()> {
    let target = Target::parse(&args.to)?;

    // Fail before transferring anything if SSH is not usable, so a typo does
    // not look like a network problem halfway through.
    println!("检查 {}", target.host);
    publish::check_target(&target).context("SSH 不可用；请确认主机名、密钥和 ~/.ssh/config")?;

    let log = |message: &str| println!("  {message}");

    let report = publish::publish(&args.out, &target, args.dry_run, &log)?;

    println!();
    if args.dry_run {
        println!("演练完成: {} 个文件会变化", report.transferred);
    } else {
        println!(
            "已发布 {} 个文件到 {}:{}",
            report.transferred, report.target, report.path
        );
        println!("服务端会在大约 5 秒内自动重新加载，无需重启。");
    }

    Ok(())
}

fn doctor(args: &DoctorArgs) -> Result<()> {
    let mut problems = 0;

    println!("tblogs-admin doctor");
    println!();

    // --- content directory -------------------------------------------------
    println!("内容目录");
    match &args.content_dir {
        Some(dir) => {
            if !dir.is_dir() {
                println!("  ✗ {} 不是一个目录", dir.display());
                problems += 1;
            } else {
                match content::scan(dir) {
                    Ok(files) if files.is_empty() => {
                        println!("  ! {} 里没有 Markdown 文件", dir.display());
                    }
                    Ok(files) => {
                        println!("  ✓ {} 个 Markdown 文件", files.len());
                        if let Ok(count) = package::count_assets(dir)
                            && count > 0
                        {
                            println!("  ✓ 额外 {count} 个可发布资源");
                        }
                    }
                    Err(error) => {
                        println!("  ✗ 无法读取: {error:#}");
                        problems += 1;
                    }
                }
            }
        }
        None => println!("  - 未指定（用 --content-dir 或 TBLOGS_CONTENT_DIR）"),
    }

    // --- required tools ----------------------------------------------------
    println!();
    println!("依赖工具");
    for tool in ["rsync", "ssh"] {
        match find_in_path(tool) {
            Some(path) => println!("  ✓ {tool} ({})", path.display()),
            None => {
                println!("  ✗ 找不到 {tool}");
                problems += 1;
            }
        }
    }

    // --- publish target ----------------------------------------------------
    println!();
    println!("发布目标");
    match &args.to {
        Some(spec) => match Target::parse(spec) {
            Ok(target) => {
                println!("  {} → {}:{}", target.spec, target.host, target.path);
                match publish::check_target(&target) {
                    Ok(()) => println!("  ✓ SSH 可用"),
                    Err(error) => {
                        println!("  ✗ {error:#}");
                        problems += 1;
                    }
                }
            }
            Err(error) => {
                println!("  ✗ {error:#}");
                problems += 1;
            }
        },
        None => println!("  - 未指定（用 --to 或 TBLOGS_TARGET）"),
    }

    println!();
    if problems == 0 {
        println!("没有发现问题。");
        Ok(())
    } else {
        anyhow::bail!("{problems} 项需要处理")
    }
}

/// Look for an executable on `PATH`.
///
/// Searching `PATH` rather than running the tool with a version flag: not every
/// tool accepts one (`ssh` rejects `--version`, for instance), and a
/// non-standard flag would make an installed tool look missing.
fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;

    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::metadata(path)
            .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }

    #[cfg(not(unix))]
    {
        path.is_file()
    }
}
