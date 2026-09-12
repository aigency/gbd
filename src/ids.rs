//! Parse GitHub issue identifiers: `142`, `#142`, `owner/repo#142`.

use anyhow::{bail, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueRef {
    pub repo: Option<String>,
    pub number: u64,
}

impl IssueRef {
    pub fn parse(raw: &str) -> Result<Self> {
        let s = raw.trim();
        if let Some((repo, num)) = s.rsplit_once('#') {
            let number: u64 = num
                .parse()
                .map_err(|_| anyhow::anyhow!("not an issue number: {raw}"))?;
            let repo = repo.trim();
            if repo.is_empty() {
                return Ok(Self { repo: None, number });
            }
            if !repo.contains('/') {
                bail!("cross-repo id must be owner/repo#n, got {raw}");
            }
            return Ok(Self {
                repo: Some(repo.to_string()),
                number,
            });
        }
        let number: u64 = s
            .parse()
            .map_err(|_| anyhow::anyhow!("not an issue number: {raw}"))?;
        Ok(Self { repo: None, number })
    }

    pub fn display(&self, current_repo: Option<&str>) -> String {
        match (&self.repo, current_repo) {
            (Some(r), Some(cur)) if r == cur => format!("#{n}", n = self.number),
            (Some(r), _) => format!("{r}#{n}", n = self.number),
            (None, _) => format!("#{n}", n = self.number),
        }
    }
}

pub fn parse_number_list(raw: &str) -> Result<Vec<u64>> {
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    raw.split(',')
        .map(|p| IssueRef::parse(p.trim()).map(|r| r.number))
        .collect()
}

/// Pull the issue number from a github.com issue URL.
pub fn number_from_url(url: &str) -> Result<u64> {
    let url = url.trim().trim_end_matches('/');
    let Some((_, last)) = url.rsplit_once('/') else {
        bail!("not an issue URL: {url}");
    };
    last.parse()
        .map_err(|_| anyhow::anyhow!("not an issue URL: {url}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_number() {
        let r = IssueRef::parse("142").unwrap();
        assert_eq!(r.number, 142);
        assert!(r.repo.is_none());
    }

    #[test]
    fn parses_hash() {
        let r = IssueRef::parse("#142").unwrap();
        assert_eq!(r.number, 142);
        assert!(r.repo.is_none());
    }

    #[test]
    fn parses_cross_repo() {
        let r = IssueRef::parse("acme/other#88").unwrap();
        assert_eq!(r.number, 88);
        assert_eq!(r.repo.as_deref(), Some("acme/other"));
    }

    #[test]
    fn number_from_github_url() {
        assert_eq!(
            number_from_url("https://github.com/acme/widgets/issues/12").unwrap(),
            12
        );
    }
}
