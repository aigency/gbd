//! Optional `.gbd.yml` in the repo root.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub memory_issue: Option<u64>,
    /// GitHub Project number under the repo's owner (the board).
    #[serde(default)]
    pub project: Option<u64>,
}

impl Config {
    pub fn project_number(&self) -> Option<u64> {
        self.project
    }

    pub fn parse(text: &str) -> Result<Self> {
        let mut cfg = Config::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once(':') else {
                continue;
            };
            let k = k.trim();
            let v = v.trim().trim_matches('"').trim_matches('\'');
            match k {
                "repo" if !v.is_empty() && v != "null" => cfg.repo = Some(v.to_string()),
                "memory_issue" if !v.is_empty() && v != "null" => {
                    cfg.memory_issue = Some(v.parse().context("memory_issue")?);
                }
                "project" if !v.is_empty() && v != "null" => {
                    cfg.project = Some(v.parse().context("project (must be the board's number)")?);
                }
                _ => {}
            }
        }
        Ok(cfg)
    }

    pub fn render(&self) -> String {
        let mut out = String::from("# gbd config. Commit this file.\n");
        if let Some(r) = &self.repo {
            let _ = writeln!(out, "repo: {r}");
        }
        if let Some(n) = self.memory_issue {
            let _ = writeln!(out, "memory_issue: {n}");
        }
        if let Some(p) = self.project {
            let _ = writeln!(out, "project: {p}");
        }
        out
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        let text =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        fs::write(path, self.render()).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}

/// Walk from `start` toward filesystem root looking for `.gbd.yml`.
pub fn find_from(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        let candidate = dir.join(".gbd.yml");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

pub fn find() -> Option<PathBuf> {
    std::env::current_dir().ok().and_then(|cwd| find_from(&cwd))
}

pub fn load() -> Result<Config> {
    match find() {
        Some(path) => Config::load_from(&path),
        None => Ok(Config::default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".gbd.yml");
        let cfg = Config {
            repo: Some("acme/widgets".into()),
            memory_issue: Some(7),
            project: None,
        };
        cfg.save_to(&path).unwrap();
        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(cfg, loaded);
    }

    #[test]
    fn parse_comments_and_quotes() {
        let cfg = Config::parse("repo: 'acme/widgets'\n# hi\nmemory_issue: 12\n").unwrap();
        assert_eq!(cfg.repo.as_deref(), Some("acme/widgets"));
        assert_eq!(cfg.memory_issue, Some(12));
    }

    #[test]
    fn project_must_be_a_number() {
        assert_eq!(Config::parse("project: 8\n").unwrap().project, Some(8));
        let err = Config::parse("project: abc\n").unwrap_err();
        assert!(format!("{err:#}").contains("project"), "{err:#}");
        assert!(Config::parse("memory_issue: abc\n").is_err());
    }

    #[test]
    fn find_walks_up() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/b");
        fs::create_dir_all(&nested).unwrap();
        fs::write(dir.path().join(".gbd.yml"), "repo: a/b\n").unwrap();
        let found = find_from(&nested).unwrap();
        assert_eq!(found, dir.path().join(".gbd.yml"));
    }
}
