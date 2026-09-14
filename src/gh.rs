//! Thin `gh` process wrapper. gbd never talks to GitHub except through `gh`.

use anyhow::{Context, Result};
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
    retrying(args, None)
}

/// Run, and when GitHub answers with its secondary rate limit (HTTP 403 or
/// 429 saying "secondary rate limit" or "abuse"), wait and try again: that
/// answer means the request was rejected, so repeating it is safe. The
/// primary hourly quota is waited out too (`wait_out_primary`). Nothing
/// else is retried, 5xx included, since a create behind a 502 may have
/// gone through; `gh issue create` is never retried at all, being several
/// mutations in one. gh does not relay the `Retry-After` header for
/// these commands, so the wait is what GitHub documents for that case:
/// at least a minute, doubling on each retry (`GBD_BACKOFF_MS` sets the
/// first wait; five retries). A `retry-after: N` in the message, when gh
/// does print one, is honoured instead.
fn retrying(args: &[&str], stdin: Option<&[u8]>) -> Result<String> {
    const RETRIES: u32 = 5;
    let base: u64 = std::env::var("GBD_BACKOFF_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60_000);
    // `gh issue create --type` is two mutations (create, then set the
    // type; parent and blocked-by add more): a limit hit on a later one
    // leaves the issue in place, so repeating the command would duplicate
    // it. That command gets one attempt; `gbd import` finds an issue whose
    // record was lost on its next run.
    let composite = args.first() == Some(&"issue") && args.get(1) == Some(&"create");
    let mut wait = base;
    let mut secondary = 0u32;
    let mut primary = 0u32;
    loop {
        let output = spawn(args, stdin)?;
        // Classified on what gh printed, never on the command line: a body
        // that happens to say "rate limit" must not turn a 502 into a retry.
        if composite || output.status.success() {
            return finish(args, &output);
        }
        if primary_limited(&output) {
            if primary >= PRIMARY_WAITS {
                return finish(args, &output);
            }
            primary += 1;
            wait_out_primary(args, primary);
            continue;
        }
        if !rate_limited(&output) || secondary >= RETRIES {
            return finish(args, &output);
        }
        secondary += 1;
        let ms = retry_after_ms(&output).unwrap_or(wait);
        eprintln!(
            "gh: rate limited; retrying in {}s ({secondary} of {RETRIES})",
            ms.div_ceil(1000)
        );
        std::thread::sleep(std::time::Duration::from_millis(ms));
        wait = wait.saturating_mul(2);
    }
}

/// How many times in a row the primary quota is waited out for one call.
/// Blind waits run a minute, doubling to fifteen, so eight of them outlast
/// any hour.
const PRIMARY_WAITS: u32 = 8;

/// The primary quota, hit for the `n`th time in a row on this call. When
/// `/rate_limit` knows a reset ahead, sleep until it. It often does not:
/// once GraphQL has begun refusing, `/rate_limit` can report the pool as
/// untouched, a full quota and a fresh hour, while GraphQL goes on
/// refusing until the real window ends (seen 2026-09-13). A reset that is
/// not ahead therefore means a blind wait: a minute, doubling, fifteen at
/// most (`GBD_BACKOFF_MS` caps it so tests do not wait).
fn wait_out_primary(args: &[&str], n: u32) {
    let (pool, reset) = match exhausted(args) {
        Ok(found) => found,
        Err(err) => {
            eprintln!(
                "gh: primary rate limit on {}; could not read the reset time ({err:#})",
                pool_of(args)
            );
            (pool_of(args), 0)
        }
    };
    if reset > unix_now() {
        wait_for_reset(pool, reset);
        return;
    }
    let cap: u64 = std::env::var("GBD_BACKOFF_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map_or(900_000, |ms| ms.min(900_000));
    let ms = (60_000u64 << (n - 1).min(4)).min(cap);
    let secs = ms / 1000;
    eprintln!(
        "gh: primary rate limit on {pool}; GitHub reports no reset ahead, waiting {}m{:02}s ({n} of {PRIMARY_WAITS})",
        secs / 60,
        secs % 60
    );
    std::thread::sleep(std::time::Duration::from_millis(ms));
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// gh's own output for a failed call, lowercased: stderr, else stdout.
fn failure_text(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let text = if stderr.trim().is_empty() {
        String::from_utf8_lossy(&output.stdout)
    } else {
        stderr
    };
    text.to_ascii_lowercase()
}

/// The secondary (abuse) limit only: it clears in seconds to minutes. The
/// primary hourly quota also says "rate limit", but waiting five short
/// backoffs for it would be pointless, so that one fails immediately.
fn rate_limited(output: &Output) -> bool {
    let msg = failure_text(output);
    // `gh api graphql` reports the secondary limit inside a 200 with no
    // status line, so the explicit wording counts on its own; the vaguer
    // "abuse" wording needs the status to back it up.
    let status = msg.contains("http 429") || msg.contains("http 403");
    msg.contains("secondary rate limit") || (status && msg.contains("abuse"))
}

/// The primary hourly quota, as opposed to the secondary limit: GitHub says
/// the limit is exceeded and does not say "secondary".
fn primary_limited(output: &Output) -> bool {
    says_primary_limit(&failure_text(output))
}

fn says_primary_limit(msg: &str) -> bool {
    msg.contains("rate limit") && msg.contains("exceeded") && !msg.contains("secondary")
}

/// Whether a failed call ran into the primary quota: for the one call
/// `retrying` never repeats (`gh issue create`), whose caller decides what
/// a repeat would mean. Read from the failure itself, never from the
/// error's text, which carries the command line: a title or body that
/// says "rate limit exceeded" must not turn a 5xx into a limit.
pub fn primary_limit_hit(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|e| e.downcast_ref::<Failure>().is_some_and(|f| f.primary_limit))
}

/// A failed `gh` call: the command line and what gh printed, classified
/// on gh's own text alone.
#[derive(Debug)]
struct Failure {
    line: String,
    primary_limit: bool,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.line)
    }
}

impl std::error::Error for Failure {}

/// The pool a call most likely draws on: GraphQL for `gh api graphql` and
/// for the `issue`, `project`, and `repo` subcommands (GraphQL
/// underneath), core otherwise. A guess; `exhausted` checks the numbers.
fn pool_of(args: &[&str]) -> &'static str {
    match args {
        ["api", "graphql", ..] | ["issue" | "project" | "repo", ..] => "graphql",
        _ => "core",
    }
}

/// `GET /rate_limit`, which does not count against any pool.
fn rate_limits() -> Result<Value> {
    let args = ["api", "rate_limit"];
    let text = finish(&args, &spawn(&args, None)?)?;
    serde_json::from_str(&text).context("reading /rate_limit")
}

/// A pool's remaining points and reset time (Unix seconds).
fn pool_budget(limits: &Value, pool: &str) -> Result<(u64, u64)> {
    let pool = limits
        .pointer(&format!("/resources/{pool}"))
        .with_context(|| format!("/rate_limit has no {pool} pool"))?;
    let field = |k: &str| {
        pool.get(k)
            .and_then(Value::as_u64)
            .with_context(|| format!("/rate_limit: no {k}"))
    };
    Ok((field("remaining")?, field("reset")?))
}

/// A pool's remaining points and reset time, freshly read.
pub fn budget(pool: &str) -> Result<(u64, u64)> {
    pool_budget(&rate_limits()?, pool)
}

/// The pool a failed call ran out of, and its reset time: whichever of
/// graphql and core is at zero, the one the command draws on first. When
/// neither is, the quota came back between the failure and this read, so
/// the reset is now and the retry follows at once.
fn exhausted(args: &[&str]) -> Result<(&'static str, u64)> {
    let limits = rate_limits()?;
    let guess = pool_of(args);
    let other = if guess == "graphql" {
        "core"
    } else {
        "graphql"
    };
    for pool in [guess, other] {
        let (remaining, reset) = pool_budget(&limits, pool)?;
        if remaining == 0 {
            return Ok((pool, reset));
        }
    }
    Ok((guess, 0))
}

/// Sleep until `reset` (Unix seconds) plus a few seconds, saying so.
/// `GBD_BACKOFF_MS`, when set, caps the margin so tests do not wait.
pub fn wait_for_reset(pool: &str, reset: u64) {
    let now = unix_now();
    let margin_ms: u64 = std::env::var("GBD_BACKOFF_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map_or(5_000, |ms| ms.min(5_000));
    let secs = reset.saturating_sub(now).min(3_700);
    eprintln!(
        "gh: primary rate limit on {pool}; waiting {}m{:02}s for the reset",
        secs / 60,
        secs % 60
    );
    std::thread::sleep(std::time::Duration::from_millis(secs * 1000 + margin_ms));
}

/// `retry-after: 30` → 30 000 ms, when gh relays the header.
fn retry_after_ms(output: &Output) -> Option<u64> {
    let msg = failure_text(output);
    let idx = msg.find("retry-after:")?;
    let secs: String = msg[idx + "retry-after:".len()..]
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    secs.parse::<u64>().ok().map(|s| s.saturating_mul(1000))
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
        return Err(Failure {
            primary_limit: says_primary_limit(&msg.to_ascii_lowercase()),
            line: format!("gh {} failed: {msg}", args.join(" ")),
        }
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Run `gh` with `stdin` piped in (for `--body-file -` and friends).
pub fn run_stdin(args: &[&str], stdin: &[u8]) -> Result<String> {
    retrying(args, Some(stdin))
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
    retrying(&str_args, stdin.as_deref())
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
    #[test]
    fn primary_limit_is_read_from_the_failure_not_the_error_text() {
        let output = |stderr: &str| Output {
            status: std::process::ExitStatus::default(),
            stdout: Vec::new(),
            stderr: stderr.as_bytes().to_vec(),
        };
        // A body that says the words, behind a 5xx: not the limit.
        let args = [
            "issue",
            "create",
            "--body",
            "API rate limit exceeded, retry",
        ];
        let err = finish(
            &args,
            &Output {
                status: failed(),
                ..output("HTTP 502: Bad Gateway")
            },
        )
        .unwrap_err()
        .context("creating issue");
        assert!(!primary_limit_hit(&err), "{err:#}");
        // gh's own text says it: the limit, through any context.
        let err = finish(
            &args,
            &Output {
                status: failed(),
                ..output("GraphQL: API rate limit already exceeded for user ID 1")
            },
        )
        .unwrap_err()
        .context("creating issue");
        assert!(primary_limit_hit(&err), "{err:#}");
    }

    fn failed() -> std::process::ExitStatus {
        std::process::Command::new("false").status().unwrap()
    }

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
