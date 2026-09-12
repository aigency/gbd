//! Thin `gh` process wrapper. gbd never talks to GitHub except through `gh`.

use anyhow::{anyhow, Context, Result};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::io::Write;
use std::process::{Command, Output, Stdio};

/// Issue fields (and related REST) need a current API version.
pub const API_VERSION: &str = "2026-03-10";

fn base_command() -> Command {
    let mut cmd = Command::new("gh");
    cmd.env("GH_PROMPT", "never")
        .env("GH_PAGER", "cat")
        .env("GH_NO_UPDATE_NOTIFIER", "1");
    cmd
}

/// Run `gh` with the given args. Returns stdout (trimmed) on success.
pub fn run(args: &[&str]) -> Result<String> {
    finish(args, &spawn(args, None)?)
}

fn finish(args: &[&str], output: &Output) -> Result<String> {
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let msg = if stderr.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            stderr.trim().to_string()
        };
        return Err(anyhow!("gh {} failed: {msg}", args.join(" ")));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Run `gh` with `stdin` piped in (for `--body-file -` and friends).
pub fn run_stdin(args: &[&str], stdin: &[u8]) -> Result<String> {
    finish(args, &spawn(args, Some(stdin))?)
}

pub fn run_json<T: DeserializeOwned>(args: &[&str]) -> Result<T> {
    let stdout = run(args)?;
    decode(&stdout, || format!("gh {}", args.join(" ")))
}

fn decode<T: DeserializeOwned>(stdout: &str, what: impl Fn() -> String) -> Result<T> {
    serde_json::from_str(stdout).with_context(|| {
        format!(
            "decoding JSON from `{}` (got {} bytes)",
            what(),
            stdout.len()
        )
    })
}

/// `gh api graphql`. Variables are passed with `-F` so numbers stay numbers.
/// Returns the full response envelope (`data` / `errors`).
pub fn graphql(query: &str, vars: &[(&str, &str)]) -> Result<Value> {
    let mut args: Vec<String> = vec![
        "api".into(),
        "graphql".into(),
        "-H".into(),
        "GraphQL-Features: issue_fields".into(),
        "-f".into(),
        format!("query={query}"),
    ];
    for (k, v) in vars {
        args.push("-F".into());
        args.push(format!("{k}={v}"));
    }
    let str_args: Vec<&str> = args.iter().map(String::as_str).collect();
    let stdout = run(&str_args)?;
    decode(&stdout, || "gh api graphql".to_string())
}

/// REST via `gh api`. `body` is sent as JSON on stdin (`--input -`).
pub fn api(method: &str, path: &str, body: Option<&Value>) -> Result<String> {
    let mut args: Vec<String> = vec![
        "api".into(),
        "--method".into(),
        method.into(),
        "-H".into(),
        "Accept: application/vnd.github+json".into(),
        "-H".into(),
        format!("X-GitHub-Api-Version: {API_VERSION}"),
        path.trim_start_matches('/').to_string(),
    ];
    let stdin = match body {
        Some(v) => Some(serde_json::to_vec(v).context("encoding JSON body")?),
        None => None,
    };
    if stdin.is_some() {
        args.push("--input".into());
        args.push("-".into());
    }
    let str_args: Vec<&str> = args.iter().map(String::as_str).collect();
    finish(&str_args, &spawn(&str_args, stdin.as_deref())?)
}

pub fn api_json<T: DeserializeOwned>(method: &str, path: &str, body: Option<&Value>) -> Result<T> {
    let stdout = api(method, path, body)?;
    decode(&stdout, || format!("gh api {method} {path}"))
}

/// The oldest GitHub CLI gbd works with: 2.94.0 added `gh issue create
/// --type / --parent / --blocked-by / --blocking` and the matching `edit`
/// flags (June 2026).
pub const MIN_GH_VERSION: (u32, u32, u32) = (2, 94, 0);

/// Installed `gh` version, or None if `gh` is missing or its output is unexpected.
pub fn version() -> Option<(u32, u32, u32)> {
    let out = Command::new("gh")
        .arg("--version")
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    parse_version(&String::from_utf8_lossy(&out.stdout))
}

/// `gh version 2.100.0 (2026-09-03)` → (2, 100, 0). Tolerates a `-suffix`.
pub fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let token = text.lines().next()?.split_whitespace().nth(2)?;
    let core = token.split(['-', '+']).next()?;
    let mut parts = core.split('.').map(str::parse::<u32>);
    Some((
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next().unwrap_or(Ok(0)).ok()?,
    ))
}

pub fn version_string(v: (u32, u32, u32)) -> String {
    format!("{}.{}.{}", v.0, v.1, v.2)
}

pub fn gh_on_path() -> bool {
    version().is_some()
}

pub fn auth_logged_in() -> bool {
    Command::new("gh")
        .args(["auth", "status"])
        .env("GH_PROMPT", "never")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Spawn `gh` with stdout and stderr captured. `Command::spawn` inherits the
/// parent's stdio by default, which would print gh's output to the terminal
/// and hand us an empty string — every JSON-reading command depends on this.
fn spawn(args: &[&str], stdin: Option<&[u8]>) -> Result<Output> {
    let mut cmd = base_command();
    cmd.args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    let mut child = cmd.spawn().with_context(|| {
        format!(
            "failed to execute `gh {}` — is gh on PATH? brew install gh",
            args.join(" ")
        )
    })?;
    if let Some(data) = stdin {
        // Scoped so the pipe closes (EOF) before we wait; gh blocks otherwise.
        let mut pipe = child.stdin.take().context("opening gh stdin")?;
        pipe.write_all(data).context("writing JSON to gh stdin")?;
    }
    child.wait_with_output().context("waiting for gh")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gh_on_path_does_not_panic() {
        let _ = gh_on_path();
    }

    #[test]
    fn parses_gh_version_line() {
        assert_eq!(
            parse_version("gh version 2.100.0 (2026-09-03)\nhttps://…"),
            Some((2, 100, 0))
        );
        assert_eq!(parse_version("gh version 2.94.0"), Some((2, 94, 0)));
        assert_eq!(
            parse_version("gh version 2.95.0-rc.1 (…)"),
            Some((2, 95, 0))
        );
        assert_eq!(parse_version("gh version 0.0.0-fake"), Some((0, 0, 0)));
        assert_eq!(parse_version("something else"), None);
        assert!((2, 94, 0) >= MIN_GH_VERSION);
        assert!((2, 93, 9) < MIN_GH_VERSION);
    }
}
