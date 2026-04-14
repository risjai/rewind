use colored::Colorize;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

const GITHUB_RELEASES_URL: &str =
    "https://api.github.com/repos/agentoptics/rewind/releases/latest";
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

fn cache_path() -> PathBuf {
    let base = std::env::var("REWIND_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".rewind")
        });
    base.join(".update-check")
}

fn should_check() -> bool {
    let path = cache_path();
    match fs::metadata(&path) {
        Ok(meta) => match meta.modified() {
            Ok(modified) => modified.elapsed().unwrap_or(CHECK_INTERVAL) >= CHECK_INTERVAL,
            Err(_) => true,
        },
        Err(_) => true,
    }
}

fn write_cache(latest_version: &str) {
    let path = cache_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(&path, latest_version);
}

fn read_cache() -> Option<String> {
    fs::read_to_string(cache_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| is_valid_version(s))
}

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn is_valid_version(v: &str) -> bool {
    parse_version(v).is_some()
}

fn parse_version(v: &str) -> Option<(u32, u32, u32)> {
    let v = v.strip_prefix('v').unwrap_or(v);
    let parts: Vec<&str> = v.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    Some((
        parts[0].parse().ok()?,
        parts[1].parse().ok()?,
        parts[2].parse().ok()?,
    ))
}

pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

fn is_ci() -> bool {
    std::env::var("CI").is_ok()
        || std::env::var("GITHUB_ACTIONS").is_ok()
        || std::env::var("JENKINS_URL").is_ok()
        || std::env::var("GITLAB_CI").is_ok()
}

/// Print an update notice if a newer version is cached, then spawn a
/// background task to refresh the cache. The notice is always printed
/// synchronously (before command output) from cached data. The network
/// fetch only updates the cache file — it never prints.
pub fn check_for_updates_background() {
    if std::env::var("REWIND_NO_UPDATE_CHECK").is_ok() || is_ci() {
        return;
    }

    // Always try to print from cache first (synchronous, before command output)
    if let Some(cached) = read_cache()
        && is_newer(&cached, current_version())
    {
        print_update_notice(&cached);
    }

    // Refresh cache in background if stale (network fetch only, no printing)
    if should_check() {
        tokio::spawn(async {
            if let Err(e) = refresh_cache().await {
                tracing::debug!("Update check failed (non-fatal): {}", e);
            }
        });
    }
}

async fn refresh_cache() -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent("rewind-cli")
        .build()?;

    let resp = client.get(GITHUB_RELEASES_URL).send().await?;
    let resp = resp.error_for_status()?;
    let json: serde_json::Value = resp.json().await?;

    let tag = json["tag_name"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing tag_name"))?;
    let version = tag.trim_start_matches('v');

    if is_valid_version(version) {
        write_cache(version);
    }

    Ok(())
}

fn print_update_notice(latest: &str) {
    eprintln!(
        "\n  {} Rewind {} available (current: {})",
        "⬆".cyan(),
        format!("v{}", latest).green().bold(),
        format!("v{}", current_version()).dimmed(),
    );
    eprintln!(
        "  {} curl -fsSL https://raw.githubusercontent.com/agentoptics/rewind/master/install.sh | sh\n",
        "Run:".dimmed(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_newer_basic() {
        assert!(is_newer("0.12.0", "0.11.0"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(is_newer("0.11.1", "0.11.0"));
    }

    #[test]
    fn test_is_newer_equal() {
        assert!(!is_newer("0.11.0", "0.11.0"));
    }

    #[test]
    fn test_is_newer_older() {
        assert!(!is_newer("0.10.0", "0.11.0"));
        assert!(!is_newer("0.11.0", "0.12.0"));
    }

    #[test]
    fn test_is_newer_with_v_prefix() {
        assert!(is_newer("v0.12.0", "0.11.0"));
        assert!(is_newer("v0.12.0", "v0.11.0"));
    }

    #[test]
    fn test_parse_version_valid() {
        assert_eq!(parse_version("0.11.0"), Some((0, 11, 0)));
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
    }

    #[test]
    fn test_parse_version_invalid() {
        assert_eq!(parse_version("abc"), None);
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn test_parse_version_prerelease_rejected() {
        assert_eq!(parse_version("0.12.0-beta.1"), None);
        assert_eq!(parse_version("0.12.0-rc1"), None);
    }

    #[test]
    fn test_is_valid_version() {
        assert!(is_valid_version("0.11.0"));
        assert!(is_valid_version("v1.2.3"));
        assert!(!is_valid_version("0.12.0-beta"));
        assert!(!is_valid_version("garbage"));
        assert!(!is_valid_version(""));
    }

    #[test]
    fn test_current_version_is_valid() {
        assert!(parse_version(current_version()).is_some());
    }

    #[test]
    fn test_is_ci_detection() {
        // In test environments CI is often set, but we just verify the function doesn't panic
        let _ = is_ci();
    }
}
