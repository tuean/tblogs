//! Publishing a content package to a remote host.
//!
//! Transport is deliberately plain: create the destination directory over SSH,
//! then `rsync` the package into it. That keeps the server free of any upload
//! endpoint — there is no write path to attack, and the published state is
//! simply whatever files are on disk.
//!
//! `rsync` replaces files by writing a temporary name and renaming it into
//! place, so the running server either sees the old `site.db` or the new one,
//! never a partial file. It notices the change and reloads by itself.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};

/// A `user@host:/path` destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The original specification, for messages.
    pub spec: String,
    /// `user@host`, or just `host`, as SSH understands it.
    pub host: String,
    /// Absolute destination directory on the remote machine.
    pub path: String,
}

impl Target {
    /// Parse `user@host:/path`.
    ///
    /// The colon separates host from path, so the path may contain colons but
    /// the host may not — which matches how SSH itself reads the argument.
    pub fn parse(spec: &str) -> Result<Self> {
        let spec = spec.trim();

        let (host, path) = spec
            .split_once(':')
            .with_context(|| format!("target must look like user@host:/path (got {spec:?})"))?;

        if host.is_empty() {
            anyhow::bail!("target {spec:?} has no host");
        }
        if path.is_empty() {
            anyhow::bail!("target {spec:?} has no destination path");
        }

        Ok(Self {
            spec: spec.to_string(),
            host: host.to_string(),
            path: path.to_string(),
        })
    }

    /// The `host:path` form rsync expects.
    fn rsync_spec(&self) -> String {
        format!("{}:{}", self.host, self.path)
    }
}

/// What a publish run did.
#[derive(Debug, Clone)]
pub struct PublishReport {
    pub target: String,
    pub path: String,
    pub transferred: usize,
    pub bytes: u64,
}

/// Copy the contents of `dist` to the target.
///
/// Only the files that make up a package are sent. Anything else in `dist` —
/// notably the `.staging` scratch directory — is excluded.
pub fn publish(
    dist: &Path,
    target: &Target,
    dry_run: bool,
    log: &dyn Fn(&str),
) -> Result<PublishReport> {
    if !dist.join("site.db").is_file() {
        anyhow::bail!(
            "{} has no site.db; run `tblogs-admin build` first",
            dist.display()
        );
    }

    if !dist.join("server").is_dir() {
        log(
            "note: dist/server is missing; run `tblogs-admin bundle` to refresh the deployment files",
        );
    }

    if !dry_run {
        log(&format!("creating {} on {}", target.path, target.host));
        let mut ssh = Command::new("ssh");
        // `mkdir -p` is done over SSH because macOS's rsync has no `--mkpath`.
        ssh.arg(&target.host)
            .arg(format!("mkdir -p {}", shell_quote(&target.path)));
        run(&mut ssh).context("could not create the destination directory")?;
    }

    let mut rsync = Command::new("rsync");
    rsync
        .arg("--archive")
        .arg("--compress")
        .arg("--human-readable")
        .arg("--stats")
        // Avoid a window where the remote directory is missing files: rsync
        // uploads to temporary names and renames them at the end.
        .arg("--delay-updates")
        // Surface the transfer list so the operator can see what moved.
        .arg("--itemize-changes")
        // The scratch directory is local bookkeeping and must never ship.
        .arg("--exclude=.staging")
        .arg("--delete");

    if dry_run {
        rsync.arg("--dry-run");
    }

    // Trailing slashes: copy the *contents* of dist into the target directory.
    let source = format!("{}/", dist.display());
    rsync.arg(&source).arg(target.rsync_spec());

    log(&format!("syncing {} -> {}", dist.display(), target.spec));
    let output = run(&mut rsync).with_context(|| {
        format!(
            "rsync to {} failed — is rsync installed on both machines, and is the SSH key set up?",
            target.spec
        )
    })?;

    let transferred = count_itemized(&output);
    let bytes = parse_transferred_bytes(&output);

    Ok(PublishReport {
        target: target.host.clone(),
        path: target.path.clone(),
        transferred,
        bytes,
    })
}

/// Run a command, returning its output, and fail with the captured text.
fn run(command: &mut Command) -> Result<String> {
    let output = command
        .output()
        .with_context(|| format!("could not run {}", command.get_program().to_string_lossy()))?;

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    if !output.status.success() {
        let code = output
            .status
            .code()
            .map(|code| code.to_string())
            .unwrap_or_else(|| "signal".to_string());

        anyhow::bail!(
            "{} exited with {}\n{}{}",
            command.get_program().to_string_lossy(),
            code,
            if stderr.trim().is_empty() {
                String::new()
            } else {
                format!("stderr: {}\n", stderr.trim())
            },
            if stdout.trim().is_empty() {
                String::new()
            } else {
                format!("stdout: {}", stdout.trim())
            }
        );
    }

    Ok(format!("{stdout}{stderr}"))
}

/// Count the files rsync reported as changed.
fn count_itemized(output: &str) -> usize {
    output
        .lines()
        .filter(|line| {
            // rsync's itemised output looks like `>f+++++++++ path`; the stats
            // block uses a different shape and is filtered out by the length
            // and prefix check.
            line.len() > 12
                && line.starts_with(['>', '<', 'c', 'h', '.'])
                && !line.contains("sending incremental")
        })
        .count()
}

/// Pull the transferred byte count out of rsync's `--stats` block.
fn parse_transferred_bytes(output: &str) -> u64 {
    for line in output.lines() {
        let line = line.trim();
        if line.starts_with("Total transferred file size:")
            && let Some(number) = line.split(':').nth(1)
        {
            let digits: String = number
                .trim()
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == ',')
                .filter(|c| c.is_ascii_digit())
                .collect();
            if let Ok(value) = digits.parse() {
                return value;
            }
        }
    }
    0
}

/// Quote a string for a POSIX shell.
fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '~'))
    {
        return value.to_string();
    }

    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Check that the target host is reachable and that SSH works non-interactively.
///
/// Used by `doctor` so a publish that would prompt for a password or fail on a
/// typo is caught beforehand.
pub fn check_target(target: &Target) -> Result<()> {
    let mut ssh = Command::new("ssh");
    ssh.arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=10")
        .arg(&target.host)
        .arg("true");

    run(&mut ssh).with_context(|| format!("could not reach {} over SSH", target.host))?;

    Ok(())
}

/// The default output directory for `build`.
pub fn default_dist_dir() -> PathBuf {
    PathBuf::from("dist")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_usual_target_forms() {
        let target = Target::parse("deploy@example.com:/srv/tblogs").unwrap();
        assert_eq!(target.host, "deploy@example.com");
        assert_eq!(target.path, "/srv/tblogs");
        assert_eq!(target.rsync_spec(), "deploy@example.com:/srv/tblogs");

        // Bare host, and a path containing a colon.
        let target = Target::parse("myhost:/var/lib/tblogs").unwrap();
        assert_eq!(target.host, "myhost");
        assert_eq!(target.path, "/var/lib/tblogs");

        let target = Target::parse("host:/srv/weird:name").unwrap();
        assert_eq!(target.path, "/srv/weird:name");
    }

    #[test]
    fn rejects_incomplete_targets() {
        for spec in ["", "justhost", ":/path", "host:"] {
            assert!(
                Target::parse(spec).is_err(),
                "{spec:?} should have been rejected"
            );
        }
    }

    #[test]
    fn quotes_shell_arguments() {
        assert_eq!(shell_quote("/srv/tblogs"), "/srv/tblogs");
        assert_eq!(shell_quote("/srv/my blogs"), "'/srv/my blogs'");
        assert_eq!(shell_quote("a'b"), r"'a'\''b'");
    }

    #[test]
    fn reads_rsync_stats() {
        let output = "\
>f+++++++++ site.db
>f+++++++++ media/a.png

Number of files: 3
Total transferred file size: 12,345 bytes
";
        assert_eq!(count_itemized(output), 2);
        assert_eq!(parse_transferred_bytes(output), 12345);

        assert_eq!(parse_transferred_bytes("nothing here"), 0);
        assert_eq!(count_itemized(""), 0);
    }
}
