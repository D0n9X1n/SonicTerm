use std::time::Duration;

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateCheckResult {
    Newer { tag: String, url: String },
    UpToDate,
    Unavailable,
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

pub fn check_latest_release(current: &str) -> UpdateCheckResult {
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(6)).build();
    let response = agent
        .get("https://api.github.com/repos/D0n9X1n/SonicTerm/releases")
        .set("User-Agent", "SonicTerm")
        .set("Accept", "application/vnd.github+json")
        .call();
    let Ok(response) = response else {
        return UpdateCheckResult::Unavailable;
    };
    let Ok(body) = response.into_string() else {
        return UpdateCheckResult::Unavailable;
    };
    latest_release_from_json(current, &body).unwrap_or(UpdateCheckResult::Unavailable)
}

pub fn latest_release_from_json(current: &str, body: &str) -> Option<UpdateCheckResult> {
    let releases: Vec<GithubRelease> = serde_json::from_str(body).ok()?;
    let latest = releases.into_iter().find(|release| !release.draft && !release.prerelease)?;
    if version_is_newer(current, &latest.tag_name) {
        Some(UpdateCheckResult::Newer { tag: latest.tag_name, url: latest.html_url })
    } else {
        Some(UpdateCheckResult::UpToDate)
    }
}

pub fn version_is_newer(current: &str, candidate: &str) -> bool {
    parse_version(candidate)
        .zip(parse_version(current))
        .is_some_and(|(candidate, current)| candidate > current)
}

fn parse_version(raw: &str) -> Option<(u64, u64, u64)> {
    let raw = raw.trim().strip_prefix('v').unwrap_or(raw.trim());
    let mut parts = raw.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    Some((major, minor, patch))
}
