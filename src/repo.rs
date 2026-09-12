//! Infer owner/repo from `--repo`, `.gbd.yml` (via the caller), `gh`, or the git remote.

use anyhow::{bail, Context, Result};

use crate::gh;

#[derive(Debug, Clone)]
pub struct Repo {
    pub name_with_owner: String,
}

impl Repo {
    pub fn owner(&self) -> &str {
        self.name_with_owner
            .split_once('/')
            .map_or(&self.name_with_owner, |(o, _)| o)
    }

    pub fn name(&self) -> &str {
        self.name_with_owner
            .split_once('/')
            .map_or(&self.name_with_owner, |(_, n)| n)
    }
}

/// `--repo`, then `.gbd.yml` `repo:` (the caller passes it), then `gh repo
/// view`, then the git remote.
pub fn resolve(explicit: Option<&str>) -> Result<Repo> {
    if let Some(r) = explicit {
        return Ok(Repo {
            name_with_owner: r.trim().to_string(),
        });
    }
    if gh::gh_on_path() {
        if let Ok(v) =
            gh::run_json::<serde_json::Value>(&["repo", "view", "--json", "nameWithOwner"])
        {
            if let Some(s) = v.get("nameWithOwner").and_then(|x| x.as_str()) {
                return Ok(Repo {
                    name_with_owner: s.to_string(),
                });
            }
        }
    }
    infer_from_git().context("could not infer owner/repo (pass --repo OWNER/REPO)")
}

fn infer_from_git() -> Result<Repo> {
    let output = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output()
        .context("git remote get-url origin")?;
    if !output.status.success() {
        bail!("no git remote 'origin'");
    }
    let url = String::from_utf8_lossy(&output.stdout);
    parse_github_remote(url.trim())
}

pub fn parse_github_remote(url: &str) -> Result<Repo> {
    let url = url.trim().trim_end_matches(".git");
    // git@github.com:owner/repo or https://github.com/owner/repo
    let rest = if let Some(r) = url.strip_prefix("git@github.com:") {
        r
    } else if let Some(r) = url.strip_prefix("https://github.com/") {
        r
    } else if let Some(r) = url.strip_prefix("ssh://git@github.com/") {
        r
    } else {
        bail!("origin is not a GitHub remote: {url}");
    };
    if !rest.contains('/') {
        bail!("not owner/repo: {rest}");
    }
    Ok(Repo {
        name_with_owner: rest.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_remote() {
        let r = parse_github_remote("https://github.com/acme/widgets.git").unwrap();
        assert_eq!(r.name_with_owner, "acme/widgets");
        assert_eq!(r.owner(), "acme");
        assert_eq!(r.name(), "widgets");
    }

    #[test]
    fn ssh_remote() {
        let r = parse_github_remote("git@github.com:acme/other.git").unwrap();
        assert_eq!(r.name_with_owner, "acme/other");
    }
}
