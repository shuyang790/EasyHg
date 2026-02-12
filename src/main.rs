mod app;
mod config;
mod domain;
mod hg;
mod ui;

use anyhow::{Result, bail};

const APP_NAME: &str = env!("CARGO_PKG_NAME");
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const HELP_TEXT: &str = "\
easyhg - lazygit-style terminal UI for Mercurial

USAGE:
  easyhg [OPTIONS]

OPTIONS:
  -h, --help       Print help and exit
  -V, --version    Print version and exit
";

#[derive(Debug)]
enum CliMode {
    RunTui,
    PrintHelp,
    PrintVersion,
}

fn parse_cli_mode<I, S>(args: I) -> Result<CliMode>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut mode = CliMode::RunTui;
    for arg in args.into_iter().skip(1).map(Into::into) {
        match arg.as_str() {
            "-h" | "--help" => mode = CliMode::PrintHelp,
            "-V" | "--version" => mode = CliMode::PrintVersion,
            other => bail!("unknown option: {other}\n\n{HELP_TEXT}"),
        }
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
        CliMode::RunTui => {
            let config = config::load_config();
            app::run_app(config).await
        }
    }
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
    fn parse_unknown_rejected() {
        let err = parse_cli_mode(argv(&["easyhg", "--bogus"])).expect_err("unknown rejected");
        assert!(err.to_string().contains("unknown option: --bogus"));
    }
}
