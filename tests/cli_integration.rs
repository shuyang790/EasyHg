use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()))
}

fn run_hg(repo: &Path, args: &[&str]) {
    let output = Command::new("hg")
        .current_dir(repo)
        .args(args)
        .output()
        .expect("spawn hg");
    assert!(
        output.status.success(),
        "hg {} failed\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn easyhg_bin() -> String {
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_easyhg") {
        return path;
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .join("target")
        .join("debug")
        .join("easyhg")
        .display()
        .to_string()
}

#[test]
fn snapshot_json_reports_modified_files() {
    if Command::new("hg").arg("--version").output().is_err() {
        eprintln!("skipping integration test: hg binary unavailable");
        return;
    }

    let repo = temp_dir("easyhg-cli-snapshot");
    fs::create_dir_all(&repo).expect("create repo dir");

    run_hg(&repo, &["init"]);
    fs::write(repo.join("a.txt"), "base\n").expect("write base");
    run_hg(&repo, &["add", "a.txt"]);
    run_hg(
        &repo,
        &["commit", "-m", "init", "-u", "tester <tester@local>"],
    );
    fs::write(repo.join("a.txt"), "base\nchanged\n").expect("mutate file");

    let output = Command::new(easyhg_bin())
        .current_dir(&repo)
        .arg("--snapshot-json")
        .output()
        .expect("run easyhg --snapshot-json");

    assert!(
        output.status.success(),
        "snapshot-json failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("parse json");
    assert_eq!(json["ok"], true);
    assert!(json["snapshot"]["files"].as_array().is_some());
    assert!(
        !json["snapshot"]["files"]
            .as_array()
            .expect("files array")
            .is_empty()
    );

    fs::remove_dir_all(&repo).ok();
}

#[test]
fn check_config_returns_non_zero_for_invalid_config() {
    let home = temp_dir("easyhg-cli-config");
    let mac_path = home
        .join("Library")
        .join("Application Support")
        .join("easyhg");
    let xdg_path = home.join(".config").join("easyhg");
    fs::create_dir_all(&mac_path).expect("create mac config path");
    fs::create_dir_all(&xdg_path).expect("create xdg config path");

    let raw = r#"
theme = "neon"

[keybinds]
commit = "meta+x"
"#;
    fs::write(mac_path.join("config.toml"), raw).expect("write mac config");
    fs::write(xdg_path.join("config.toml"), raw).expect("write xdg config");

    let output = Command::new(easyhg_bin())
        .arg("--check-config")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .output()
        .expect("run easyhg --check-config");

    assert_eq!(output.status.code(), Some(2));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).expect("parse json");
    assert_eq!(json["ok"], false);
    assert!(json["issues"].as_array().is_some());
    assert!(!json["issues"].as_array().expect("issues").is_empty());

    fs::remove_dir_all(&home).ok();
}
