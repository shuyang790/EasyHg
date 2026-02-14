mod actions;
mod app;
mod config;
mod domain;
mod hg;
mod ui;

use anyhow::{Result, bail};
use async_trait::async_trait;
use chrono::Utc;
use serde::Serialize;
use std::path::Path;

use crate::domain::{HgCapabilities, RepoSnapshot};
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

#[async_trait]
trait CliModeHgClient: Send + Sync {
    async fn run_hg_args(&self, args: &[&str]) -> Result<crate::hg::CommandResult>;
    async fn detect_capabilities(&self) -> HgCapabilities;
    async fn refresh_snapshot(&self, options: SnapshotOptions) -> Result<RepoSnapshot>;
}

#[async_trait]
impl CliModeHgClient for CliHgClient {
    async fn run_hg_args(&self, args: &[&str]) -> Result<crate::hg::CommandResult> {
        self.run_hg(args).await
    }

    async fn detect_capabilities(&self) -> HgCapabilities {
        CliHgClient::detect_capabilities(self).await
    }

    async fn refresh_snapshot(&self, options: SnapshotOptions) -> Result<RepoSnapshot> {
        HgClient::refresh_snapshot(self, options).await
    }
}

fn output_exit_code(ok: bool) -> i32 {
    if ok { 0 } else { 2 }
}

fn check_config_output(report: config::ConfigLoadReport) -> CheckConfigOutput {
    CheckConfigOutput {
        ok: report.issues.is_empty(),
        path: report.path.map(|p| p.display().to_string()),
        issues: report.issues,
    }
}

fn run_check_config() -> i32 {
    let out = check_config_output(config::load_config_with_report());
    println!(
        "{}",
        serde_json::to_string_pretty(&out).expect("serialize check config output")
    );
    output_exit_code(out.ok)
}

async fn ensure_hg_repo_for_tui(hg: &impl CliModeHgClient, cwd: &Path) -> Result<()> {
    let out = match hg.run_hg_args(&["root"]).await {
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

async fn build_snapshot_output(
    hg: &impl CliModeHgClient,
    timestamp_unix_secs: i64,
) -> SnapshotOutput {
    match hg
        .refresh_snapshot(SnapshotOptions {
            revision_limit: 200,
            include_revisions: true,
        })
        .await
    {
        Ok(snapshot) => SnapshotOutput {
            ok: true,
            timestamp_unix_secs,
            snapshot: Some(snapshot),
            error: None,
        },
        Err(err) => SnapshotOutput {
            ok: false,
            timestamp_unix_secs,
            snapshot: None,
            error: Some(err.to_string()),
        },
    }
}

async fn run_snapshot_json() -> Result<i32> {
    let cwd = std::env::current_dir()?;
    let hg = CliHgClient::new(cwd);
    let out = build_snapshot_output(&hg, Utc::now().timestamp()).await;
    println!(
        "{}",
        serde_json::to_string_pretty(&out).expect("serialize snapshot output")
    );
    Ok(output_exit_code(out.ok))
}

async fn build_doctor_output(
    hg: &impl CliModeHgClient,
    cwd: &Path,
    config_report: config::ConfigLoadReport,
    timestamp_unix_secs: i64,
) -> DoctorOutput {
    let mut probes = Vec::new();
    for args in [
        vec!["--version"],
        vec!["root"],
        vec!["status", "-Tjson"],
        vec!["log", "-l", "5", "-Tjson"],
    ] {
        let command = format!("hg {}", args.join(" "));
        match hg.run_hg_args(&args).await {
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

    let config = check_config_output(config_report);
    let probes_ok = probes.iter().all(|probe| probe.ok);
    DoctorOutput {
        ok: probes_ok && error.is_none() && config.ok,
        timestamp_unix_secs,
        cwd: cwd.display().to_string(),
        config,
        capabilities,
        repo_root,
        branch,
        probes,
        error,
    }
}

async fn run_doctor() -> Result<i32> {
    let cwd = std::env::current_dir()?;
    let hg = CliHgClient::new(cwd.clone());
    let out = build_doctor_output(
        &hg,
        &cwd,
        config::load_config_with_report(),
        Utc::now().timestamp(),
    )
    .await;
    println!(
        "{}",
        serde_json::to_string_pretty(&out).expect("serialize doctor output")
    );
    Ok(output_exit_code(out.ok))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[derive(Debug, Clone)]
    struct FakeCliModeHgClient {
        run_results: HashMap<String, std::result::Result<crate::hg::CommandResult, String>>,
        capabilities: HgCapabilities,
        snapshot_result: std::result::Result<RepoSnapshot, String>,
    }

    impl FakeCliModeHgClient {
        fn new(
            run_results: HashMap<String, std::result::Result<crate::hg::CommandResult, String>>,
            snapshot_result: std::result::Result<RepoSnapshot, String>,
        ) -> Self {
            Self {
                run_results,
                capabilities: HgCapabilities {
                    version: "hg 6.9".to_string(),
                    has_rebase: true,
                    has_histedit: true,
                    has_shelve: true,
                    supports_json_status: true,
                    supports_json_log: true,
                },
                snapshot_result,
            }
        }
    }

    #[async_trait]
    impl CliModeHgClient for FakeCliModeHgClient {
        async fn run_hg_args(&self, args: &[&str]) -> Result<crate::hg::CommandResult> {
            let key = args.join(" ");
            match self.run_results.get(&key) {
                Some(Ok(result)) => Ok(result.clone()),
                Some(Err(err)) => bail!("{err}"),
                None => bail!("missing fake result for {key}"),
            }
        }

        async fn detect_capabilities(&self) -> HgCapabilities {
            self.capabilities.clone()
        }

        async fn refresh_snapshot(&self, _options: SnapshotOptions) -> Result<RepoSnapshot> {
            self.snapshot_result
                .clone()
                .map_err(|err| anyhow::anyhow!("{err}"))
        }
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

    #[test]
    fn check_config_output_marks_issues_as_not_ok() {
        let output = check_config_output(config::ConfigLoadReport {
            config: config::AppConfig::default(),
            path: Some(PathBuf::from("/tmp/config.toml")),
            issues: vec!["bad key".to_string()],
        });
        assert!(!output.ok);
        assert_eq!(output.path, Some("/tmp/config.toml".to_string()));
        assert_eq!(output_exit_code(output.ok), 2);
    }

    #[test]
    fn check_config_output_ok_has_zero_exit_code() {
        let output = check_config_output(config::ConfigLoadReport {
            config: config::AppConfig::default(),
            path: None,
            issues: Vec::new(),
        });
        assert!(output.ok);
        assert_eq!(output_exit_code(output.ok), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ensure_repo_guard_accepts_valid_root_result() {
        let mut run_results = HashMap::new();
        run_results.insert(
            "root".to_string(),
            Ok(crate::hg::CommandResult {
                command_preview: "hg root".to_string(),
                success: true,
                stdout: "/repo\n".to_string(),
                stderr: String::new(),
            }),
        );
        let hg = FakeCliModeHgClient::new(run_results, Ok(RepoSnapshot::default()));
        ensure_hg_repo_for_tui(&hg, Path::new("/repo"))
            .await
            .expect("repo accepted");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ensure_repo_guard_reports_hg_stderr_when_not_repo() {
        let mut run_results = HashMap::new();
        run_results.insert(
            "root".to_string(),
            Ok(crate::hg::CommandResult {
                command_preview: "hg root".to_string(),
                success: false,
                stdout: String::new(),
                stderr: "abort: no repository found".to_string(),
            }),
        );
        let hg = FakeCliModeHgClient::new(run_results, Ok(RepoSnapshot::default()));
        let err = ensure_hg_repo_for_tui(&hg, Path::new("/tmp/outside"))
            .await
            .expect_err("non-repo rejected");
        assert!(
            err.to_string()
                .contains("not inside a Mercurial repository")
        );
        assert!(err.to_string().contains("abort: no repository found"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ensure_repo_guard_reports_probe_errors() {
        let mut run_results = HashMap::new();
        run_results.insert("root".to_string(), Err("spawn failed".to_string()));
        let hg = FakeCliModeHgClient::new(run_results, Ok(RepoSnapshot::default()));
        let err = ensure_hg_repo_for_tui(&hg, Path::new("/tmp/outside"))
            .await
            .expect_err("non-repo rejected");
        assert!(err.to_string().contains("spawn failed"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn snapshot_output_success_sets_snapshot_and_ok() {
        let hg = FakeCliModeHgClient::new(HashMap::new(), Ok(RepoSnapshot::default()));
        let out = build_snapshot_output(&hg, 123).await;
        assert!(out.ok);
        assert!(out.snapshot.is_some());
        assert!(out.error.is_none());
        assert_eq!(out.timestamp_unix_secs, 123);
        assert_eq!(output_exit_code(out.ok), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn snapshot_output_failure_sets_error_and_nonzero_exit() {
        let hg = FakeCliModeHgClient::new(HashMap::new(), Err("snapshot failed".to_string()));
        let out = build_snapshot_output(&hg, 124).await;
        assert!(!out.ok);
        assert!(out.snapshot.is_none());
        assert_eq!(out.error, Some("snapshot failed".to_string()));
        assert_eq!(out.timestamp_unix_secs, 124);
        assert_eq!(output_exit_code(out.ok), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn doctor_output_reports_probe_failures() {
        let mut run_results = HashMap::new();
        run_results.insert(
            "--version".to_string(),
            Ok(crate::hg::CommandResult {
                command_preview: "hg --version".to_string(),
                success: true,
                stdout: "Mercurial 6.9".to_string(),
                stderr: String::new(),
            }),
        );
        run_results.insert(
            "root".to_string(),
            Ok(crate::hg::CommandResult {
                command_preview: "hg root".to_string(),
                success: false,
                stdout: String::new(),
                stderr: "abort: no repository found".to_string(),
            }),
        );
        run_results.insert(
            "status -Tjson".to_string(),
            Ok(crate::hg::CommandResult {
                command_preview: "hg status -Tjson".to_string(),
                success: true,
                stdout: "[]".to_string(),
                stderr: String::new(),
            }),
        );
        run_results.insert(
            "log -l 5 -Tjson".to_string(),
            Ok(crate::hg::CommandResult {
                command_preview: "hg log -l 5 -Tjson".to_string(),
                success: true,
                stdout: "[]".to_string(),
                stderr: String::new(),
            }),
        );
        let hg = FakeCliModeHgClient::new(run_results, Err("snapshot failed".to_string()));
        let out = build_doctor_output(
            &hg,
            Path::new("/tmp/repo"),
            config::ConfigLoadReport {
                config: config::AppConfig::default(),
                path: None,
                issues: Vec::new(),
            },
            200,
        )
        .await;
        assert!(!out.ok);
        assert_eq!(out.timestamp_unix_secs, 200);
        assert_eq!(out.error, Some("snapshot failed".to_string()));
        assert!(out.probes.iter().any(|probe| !probe.ok));
        assert_eq!(output_exit_code(out.ok), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn doctor_output_success_when_probes_snapshot_and_config_are_ok() {
        let mut run_results = HashMap::new();
        for key in ["--version", "root", "status -Tjson", "log -l 5 -Tjson"] {
            run_results.insert(
                key.to_string(),
                Ok(crate::hg::CommandResult {
                    command_preview: format!("hg {key}"),
                    success: true,
                    stdout: String::new(),
                    stderr: String::new(),
                }),
            );
        }
        let hg = FakeCliModeHgClient::new(
            run_results,
            Ok(RepoSnapshot {
                repo_root: Some("/tmp/repo".to_string()),
                branch: Some("default".to_string()),
                ..RepoSnapshot::default()
            }),
        );
        let out = build_doctor_output(
            &hg,
            Path::new("/tmp/repo"),
            config::ConfigLoadReport {
                config: config::AppConfig::default(),
                path: Some(PathBuf::from("/tmp/config.toml")),
                issues: Vec::new(),
            },
            201,
        )
        .await;
        assert!(out.ok);
        assert_eq!(out.timestamp_unix_secs, 201);
        assert_eq!(out.repo_root, Some("/tmp/repo".to_string()));
        assert_eq!(out.branch, Some("default".to_string()));
        assert_eq!(output_exit_code(out.ok), 0);
    }
}
