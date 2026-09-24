use color_eyre::eyre::eyre;

use super::types::{ExpressionInfo, Fetcher};

/// Parse a URL string into a `Fetcher`.
///
/// Accepts forms like:
/// - `github.com/owner/repo`
/// - `https://github.com/owner/repo`
/// - `gitlab.com/owner/repo`
/// - `https://gitlab.com/owner/repo`
pub fn parse_url(url: &str) -> color_eyre::Result<Fetcher> {
    let stripped = url
        .trim()
        .trim_end_matches('/')
        .trim_start_matches("https://")
        .trim_start_matches("http://");

    let parts: Vec<&str> = stripped.splitn(4, '/').collect();

    if parts.len() < 3 {
        return Err(eyre!(
            "Could not parse URL: expected <host>/<owner>/<repo>, got: {url}"
        ));
    }

    let host = parts[0];
    let owner = parts[1].to_owned();
    let repo = parts[2].trim_end_matches(".git").to_owned();

    match host {
        "github.com" => Ok(Fetcher::GitHub { owner, repo }),
        "gitlab.com" => Ok(Fetcher::GitLab { owner, repo }),
        _ => Err(eyre!(
            "Unsupported host: {host}. Supported: github.com, gitlab.com"
        )),
    }
}

/// Mapping from GitHub API license `spdx_id` to nixpkgs license attribute name.
fn github_license_to_nix(spdx_id: &str) -> Option<&'static str> {
    match spdx_id {
        "MIT" => Some("mit"),
        "Apache-2.0" => Some("asl20"),
        "GPL-2.0" | "GPL-2.0-only" => Some("gpl2Only"),
        "GPL-2.0-or-later" => Some("gpl2Plus"),
        "GPL-3.0" | "GPL-3.0-only" => Some("gpl3Only"),
        "GPL-3.0-or-later" => Some("gpl3Plus"),
        "LGPL-2.1" | "LGPL-2.1-only" => Some("lgpl21Only"),
        "LGPL-2.1-or-later" => Some("lgpl21Plus"),
        "LGPL-3.0" | "LGPL-3.0-only" => Some("lgpl3Only"),
        "LGPL-3.0-or-later" => Some("lgpl3Plus"),
        "BSD-2-Clause" => Some("bsd2"),
        "BSD-3-Clause" => Some("bsd3"),
        "ISC" => Some("isc"),
        "MPL-2.0" => Some("mpl20"),
        "Unlicense" => Some("unlicense"),
        "AGPL-3.0" | "AGPL-3.0-only" => Some("agpl3Only"),
        "AGPL-3.0-or-later" => Some("agpl3Plus"),
        "Zlib" => Some("zlib"),
        "BSL-1.0" => Some("boost"),
        "CC0-1.0" => Some("cc0"),
        _ => None,
    }
}

/// Fetch metadata from the GitHub API and update `info` fields.
///
/// Updates: `pname`, `version`, `description`, `homepage`, `license`.
/// Only overwrites fields that are still at their default placeholder values.
pub fn fetch_github_metadata(
    owner: &str,
    repo: &str,
    info: &mut ExpressionInfo,
) -> color_eyre::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let client = reqwest::Client::builder()
            .user_agent("ekapkgs-cli")
            .build()?;

        // Fetch repo info
        let repo_url = format!("https://api.github.com/repos/{owner}/{repo}");
        let resp: serde_json::Value = client
            .get(&repo_url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        // Update pname from repo name if still default
        if info.pname == "CHANGEME" {
            if let Some(name) = resp["name"].as_str() {
                info.pname = name.to_owned();
            }
        }

        // Update description
        if info.description == "CHANGEME" {
            if let Some(desc) = resp["description"].as_str() {
                info.description = desc.to_owned();
            }
        }

        // Update homepage
        if info.homepage.is_empty() {
            info.homepage = format!("https://github.com/{owner}/{repo}");
        }

        // Update license
        if info.license == "unfree" {
            if let Some(spdx) = resp["license"]["spdx_id"].as_str() {
                if let Some(nix_license) = github_license_to_nix(spdx) {
                    info.license = nix_license.to_owned();
                }
            }
        }

        // Fetch latest release for version
        if info.version == "0.0.1" {
            let releases_url =
                format!("https://api.github.com/repos/{owner}/{repo}/releases/latest");
            if let Ok(release_resp) = client.get(&releases_url).send().await {
                if let Ok(release) = release_resp.json::<serde_json::Value>().await {
                    if let Some(tag) = release["tag_name"].as_str() {
                        info.version = tag.trim_start_matches('v').to_owned();
                    }
                }
            }

            // Fallback to tags if no releases
            if info.version == "0.0.1" {
                let tags_url =
                    format!("https://api.github.com/repos/{owner}/{repo}/tags?per_page=1");
                if let Ok(tags_resp) = client.get(&tags_url).send().await {
                    if let Ok(tags) = tags_resp.json::<Vec<serde_json::Value>>().await {
                        if let Some(tag) = tags.first().and_then(|t| t["name"].as_str()) {
                            info.version = tag.trim_start_matches('v').to_owned();
                        }
                    }
                }
            }
        }

        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_github_url() {
        let fetcher = parse_url("https://github.com/user/repo").unwrap();
        assert_eq!(
            fetcher,
            Fetcher::GitHub {
                owner: "user".into(),
                repo: "repo".into(),
            }
        );
    }

    #[test]
    fn parse_github_url_no_scheme() {
        let fetcher = parse_url("github.com/user/repo").unwrap();
        assert_eq!(
            fetcher,
            Fetcher::GitHub {
                owner: "user".into(),
                repo: "repo".into(),
            }
        );
    }

    #[test]
    fn parse_github_url_with_trailing_slash() {
        let fetcher = parse_url("https://github.com/user/repo/").unwrap();
        assert_eq!(
            fetcher,
            Fetcher::GitHub {
                owner: "user".into(),
                repo: "repo".into(),
            }
        );
    }

    #[test]
    fn parse_github_url_with_git_suffix() {
        let fetcher = parse_url("https://github.com/user/repo.git").unwrap();
        assert_eq!(
            fetcher,
            Fetcher::GitHub {
                owner: "user".into(),
                repo: "repo".into(),
            }
        );
    }

    #[test]
    fn parse_gitlab_url() {
        let fetcher = parse_url("https://gitlab.com/org/project").unwrap();
        assert_eq!(
            fetcher,
            Fetcher::GitLab {
                owner: "org".into(),
                repo: "project".into(),
            }
        );
    }

    #[test]
    fn parse_unsupported_host() {
        assert!(parse_url("https://bitbucket.org/user/repo").is_err());
    }

    #[test]
    fn parse_invalid_url() {
        assert!(parse_url("github.com/user").is_err());
    }

    #[test]
    fn license_mapping() {
        assert_eq!(github_license_to_nix("MIT"), Some("mit"));
        assert_eq!(github_license_to_nix("Apache-2.0"), Some("asl20"));
        assert_eq!(github_license_to_nix("GPL-3.0-only"), Some("gpl3Only"));
        assert_eq!(github_license_to_nix("UNKNOWN"), None);
    }
}
