mod actions;
mod app;
mod config;
mod domain;
mod hg;
mod ui;

use anyhow::{Result, bail};
use chrono::Utc;
use serde::Serialize;
use std::path::Path;

use crate::hg::{CliHgClient, HgClient, SnapshotOptions};

const APP_NAME: &str = env!("CARGO_PKG_NAME");
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const HELP_TEXT: &str = "\
easyhg - lazygit-style terminal UI for Mercurial

USAGE:
  easyhg [OPTIONS]

OPTIONS:
  -h, --help       Print help and exit
  -V, --version    Print version and exit
  --doctor         Print environment/repo diagnostics as JSON and exit
  --snapshot-json  Print current repository snapshot as JSON and exit
  --check-config   Validate config and print JSON report
";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliMode {
    RunTui,
    PrintHelp,
    PrintVersion,
    Doctor,
    SnapshotJson,
    CheckConfig,
}

fn parse_cli_mode<I, S>(args: I) -> Result<CliMode>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut mode = CliMode::RunTui;
    for arg in args.into_iter().skip(1).map(Into::into) {
        let next = match arg.as_str() {
            "-h" | "--help" => CliMode::PrintHelp,
            "-V" | "--version" => CliMode::PrintVersion,
            "--doctor" => CliMode::Doctor,
            "--snapshot-json" => CliMode::SnapshotJson,
            "--check-config" => CliMode::CheckConfig,
            other => bail!("unknown option: {other}\n\n{HELP_TEXT}"),
        };
        if mode != CliMode::RunTui && mode != next {
            bail!("options are mutually exclusive\n\n{HELP_TEXT}");
        }
        mode = next;
    }
    Ok(mode)
}

#[tokio::main]
async fn main() -> Result<()> {
    match parse_cli_mode(std::env::args())? {
        CliMode::PrintHelp => {
            println!("{HELP_TEXT}");
            Ok(())
        }
        CliMode::PrintVersion => {
            println!("{APP_NAME} {APP_VERSION}");
            Ok(())
        }
        CliMode::Doctor => {
            let exit_code = run_doctor().await?;
            std::process::exit(exit_code);
        }
        CliMode::SnapshotJson => {
            let exit_code = run_snapshot_json().await?;
            std::process::exit(exit_code);
        }
        CliMode::CheckConfig => {
            let exit_code = run_check_config();
            std::process::exit(exit_code);
        }
        CliMode::RunTui => {
            let report = config::load_config_with_report();
            let cwd = std::env::current_dir()?;
            let hg = CliHgClient::new(cwd.clone());
            if let Err(err) = ensure_hg_repo_for_tui(&hg, &cwd).await {
                eprintln!("{err}");
                std::process::exit(2);
            }
            app::run_app(report.config, report.issues).await
        }
    }
}

#[derive(Debug, Serialize)]
struct CheckConfigOutput {
    ok: bool,
    path: Option<String>,
    issues: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ProbeOutput {
    command: String,
    ok: bool,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct DoctorOutput {
    ok: bool,
    timestamp_unix_secs: i64,
    cwd: String,
    config: CheckConfigOutput,
    capabilities: Option<domain::HgCapabilities>,
    repo_root: Option<String>,
    branch: Option<String>,
    probes: Vec<ProbeOutput>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct SnapshotOutput {
    ok: bool,
    timestamp_unix_secs: i64,
    snapshot: Option<domain::RepoSnapshot>,
    error: Option<String>,
}

fn run_check_config() -> i32 {
    let report = config::load_config_with_report();
    let out = CheckConfigOutput {
        ok: report.issues.is_empty(),
        path: report.path.map(|p| p.display().to_string()),
        issues: report.issues,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&out).expect("serialize check config output")
    );
    if out.ok { 0 } else { 2 }
}

async fn ensure_hg_repo_for_tui(hg: &CliHgClient, cwd: &Path) -> Result<()> {
    let out = match hg.run_hg(&["root"]).await {
        Ok(out) => out,
        Err(err) => bail!(
            "easyhg: current directory is not inside a Mercurial repository\ncwd: {}\nhint: run this inside an hg repo (or use --doctor for diagnostics)\nerror: {}",
            cwd.display(),
            err
        ),
    };
    if out.success && !out.stdout.trim().is_empty() {
        return Ok(());
    }

    let mut message = format!(
        "easyhg: current directory is not inside a Mercurial repository\ncwd: {}\nhint: run this inside an hg repo (or use --doctor for diagnostics)",
        cwd.display()
    );
    let stderr = out.stderr.trim();
    if !stderr.is_empty() {
        message.push_str(&format!("\nhg: {}", compact_output(stderr)));
    }
    bail!("{message}");
}

fn compact_output(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

async fn run_snapshot_json() -> Result<i32> {
    let cwd = std::env::current_dir()?;
    let hg = CliHgClient::new(cwd);
    let out = match hg
        .refresh_snapshot(SnapshotOptions {
            revision_limit: 200,
            include_revisions: true,
        })
        .await
    {
        Ok(snapshot) => SnapshotOutput {
            ok: true,
            timestamp_unix_secs: Utc::now().timestamp(),
            snapshot: Some(snapshot),
            error: None,
        },
        Err(err) => SnapshotOutput {
            ok: false,
            timestamp_unix_secs: Utc::now().timestamp(),
            snapshot: None,
            error: Some(err.to_string()),
        },
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&out).expect("serialize snapshot output")
    );
    Ok(if out.ok { 0 } else { 2 })
}

async fn run_doctor() -> Result<i32> {
    let cwd = std::env::current_dir()?;
    let hg = CliHgClient::new(cwd.clone());
    let config_report = config::load_config_with_report();
    let mut probes = Vec::new();

    for args in [
        vec!["--version"],
        vec!["root"],
        vec!["status", "-Tjson"],
        vec!["log", "-l", "5", "-Tjson"],
    ] {
        let command = format!("hg {}", args.join(" "));
        match hg.run_hg(&args).await {
            Ok(result) => {
                probes.push(ProbeOutput {
                    command,
                    ok: result.success,
                    error: if result.success {
                        None
                    } else {
                        Some(result.stderr.trim().to_string())
                    },
                });
            }
            Err(err) => {
                probes.push(ProbeOutput {
                    command,
                    ok: false,
                    error: Some(err.to_string()),
                });
            }
        }
    }

    let capabilities = Some(hg.detect_capabilities().await);
    let mut repo_root = None;
    let mut branch = None;
    let mut error = None;
    match hg
        .refresh_snapshot(SnapshotOptions {
            revision_limit: 50,
            include_revisions: true,
        })
        .await
    {
        Ok(snapshot) => {
            repo_root = snapshot.repo_root;
            branch = snapshot.branch;
        }
        Err(err) => {
            error = Some(err.to_string());
        }
    }

    let config = CheckConfigOutput {
        ok: config_report.issues.is_empty(),
        path: config_report.path.map(|p| p.display().to_string()),
        issues: config_report.issues,
    };
    let probes_ok = probes.iter().all(|probe| probe.ok);
    let out = DoctorOutput {
        ok: probes_ok && error.is_none() && config.ok,
        timestamp_unix_secs: Utc::now().timestamp(),
        cwd: cwd.display().to_string(),
        config,
        capabilities,
        repo_root,
        branch,
        probes,
        error,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&out).expect("serialize doctor output")
    );
    Ok(if out.ok { 0 } else { 2 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_help() {
        let mode = parse_cli_mode(argv(&["easyhg", "--help"])).expect("help parses");
        assert!(matches!(mode, CliMode::PrintHelp));
    }

    #[test]
    fn parse_version() {
        let mode = parse_cli_mode(argv(&["easyhg", "-V"])).expect("version parses");
        assert!(matches!(mode, CliMode::PrintVersion));
    }

    #[test]
    fn parse_snapshot_json() {
        let mode = parse_cli_mode(argv(&["easyhg", "--snapshot-json"])).expect("snapshot parses");
        assert!(matches!(mode, CliMode::SnapshotJson));
    }

    #[test]
    fn parse_exclusive_options_rejected() {
        let err = parse_cli_mode(argv(&["easyhg", "--doctor", "--version"]))
            .expect_err("exclusive options rejected");
        assert!(err.to_string().contains("mutually exclusive"));
    }

    #[test]
    fn parse_unknown_rejected() {
        let err = parse_cli_mode(argv(&["easyhg", "--bogus"])).expect_err("unknown rejected");
        assert!(err.to_string().contains("unknown option: --bogus"));
    }
}
