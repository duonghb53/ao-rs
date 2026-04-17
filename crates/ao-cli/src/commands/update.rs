//! `ao-rs update` — self-upgrade + latest-version check.
//!
//! Parity target: ao-ts `ao update`
//! (`packages/cli/src/commands/update.ts` +
//! `packages/cli/src/lib/update-check.ts`), scoped to Rust distribution
//! methods (Homebrew, Cargo).
//!
//! Design goals (issue #128):
//! - `--check` always returns machine-readable JSON.
//! - Latest-version lookup prefers GitHub REST (`gh api`) over GraphQL
//!   (`gh release view`), falling back to `git ls-remote --tags`.
//! - Cached latest version with 24h TTL at
//!   `$XDG_CACHE_HOME/ao-rs/update-check.json` (or `~/.cache/...`).
//! - Homebrew vs Cargo vs unknown each resolve to a concrete recommended
//!   command.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use semver::Version;
use serde::{Deserialize, Serialize};
use tokio::process::Command;

const REPO: &str = "duonghb53/ao-rs";
const REPO_HTTPS: &str = "https://github.com/duonghb53/ao-rs.git";
const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const CACHE_FILE_NAME: &str = "update-check.json";

pub async fn update(check: bool, skip_smoke: bool, smoke_only: bool) -> Result<(), Box<dyn Error>> {
    if smoke_only {
        print_smoke_instructions();
        return Ok(());
    }

    let info = build_update_info(check).await?;

    if check {
        println!("{}", serde_json::to_string_pretty(&info)?);
        return Ok(());
    }

    let Some(latest) = info.latest_version.as_deref() else {
        return Err(
            "Unable to resolve latest version. Check `gh auth status` / network and retry.".into(),
        );
    };
    let current = info.current_version.clone();

    if !info.is_outdated {
        println!("ao-rs is up to date ({current}).");
        return Ok(());
    }

    println!("Updating ao-rs: {current} -> {latest}.");

    match info.install_method {
        InstallMethod::Homebrew => {
            run_or_explain(
                "brew",
                &["upgrade", "ao-rs"],
                Some("Homebrew upgrade failed"),
            )
            .await?
        }
        InstallMethod::Cargo => {
            run_or_explain(
                "cargo",
                &["install", "ao-cli", "--locked"],
                Some("Cargo install failed"),
            )
            .await?
        }
        InstallMethod::Unknown => {
            print_manual_instructions();
            return Ok(());
        }
    }

    if !skip_smoke {
        print_smoke_instructions();
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// UpdateInfo + orchestrator
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub current_version: String,
    pub latest_version: Option<String>,
    pub is_outdated: bool,
    pub install_method: InstallMethod,
    pub recommended_command: String,
    pub checked_at: Option<String>,
}

async fn build_update_info(force_refresh: bool) -> Result<UpdateInfo, Box<dyn Error>> {
    let current_raw = env!("CARGO_PKG_VERSION").to_string();
    let current = Version::parse(&current_raw)?;
    let method = detect_install_method(std::env::current_exe().ok().as_ref()).await;
    let recommended_command = recommended_command(method);
    let cache_file = cache_path();

    if !force_refresh {
        if let Some(cache) = read_cached_update_info(&cache_file, &current_raw, SystemTime::now()) {
            let latest_version = cache.latest_version.clone();
            let is_outdated = compare_outdated(&current, &latest_version);
            return Ok(UpdateInfo {
                current_version: current_raw,
                latest_version: Some(latest_version),
                is_outdated,
                install_method: method,
                recommended_command,
                checked_at: Some(cache.checked_at),
            });
        }
    }

    let (latest_version, checked_at) = match resolve_latest_version().await {
        Ok(v) => {
            let now = SystemTime::now();
            let checked_at = format_rfc3339_utc(now);
            let data = CacheData {
                latest_version: v.to_string(),
                checked_at: checked_at.clone(),
                current_version_at_check: current_raw.clone(),
            };
            write_cache(&cache_file, &data);
            (Some(v.to_string()), Some(checked_at))
        }
        Err(_) => (None, None),
    };

    let is_outdated = compare_outdated(&current, latest_version.as_deref().unwrap_or(""));

    Ok(UpdateInfo {
        current_version: current_raw,
        latest_version,
        is_outdated,
        install_method: method,
        recommended_command,
        checked_at,
    })
}

fn compare_outdated(current: &Version, latest: &str) -> bool {
    match Version::parse(latest) {
        Ok(l) => l > *current,
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// Install method detection
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstallMethod {
    Homebrew,
    Cargo,
    Unknown,
}

fn recommended_command(method: InstallMethod) -> String {
    match method {
        InstallMethod::Homebrew => "brew upgrade ao-rs".to_string(),
        InstallMethod::Cargo => "cargo install ao-cli --locked".to_string(),
        // For unknown, give both so `--check` JSON still surfaces a usable hint.
        InstallMethod::Unknown => "brew upgrade ao-rs   # if installed via Homebrew\n\
             cargo install ao-cli --locked   # if installed via Cargo"
            .to_string(),
    }
}

async fn detect_install_method(current_exe: Option<&PathBuf>) -> InstallMethod {
    if is_brew_managed().await {
        return InstallMethod::Homebrew;
    }

    if let Some(path) = current_exe {
        if is_cargo_bin(path) {
            return InstallMethod::Cargo;
        }
    }

    InstallMethod::Unknown
}

async fn is_brew_managed() -> bool {
    // Fast, non-destructive signal: `brew list --versions ao-rs` exits 0 if installed.
    // If brew isn't installed, spawning will error (treated as false).
    match Command::new("brew")
        .args(["list", "--versions", "ao-rs"])
        .output()
        .await
    {
        Ok(out) => out.status.success(),
        Err(_) => false,
    }
}

fn is_cargo_bin(p: &std::path::Path) -> bool {
    // Check both `~/.cargo/bin/` and `$CARGO_HOME/bin/` shapes. The former
    // covers the default install location; the latter handles users who
    // relocated `CARGO_HOME`.
    let s = p.to_string_lossy();
    if s.contains("/.cargo/bin/") {
        return true;
    }
    if let Ok(cargo_home) = std::env::var("CARGO_HOME") {
        if !cargo_home.is_empty() {
            let prefix = format!("{}/bin/", cargo_home.trim_end_matches('/'));
            if s.starts_with(&prefix) {
                return true;
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Output helpers
// ---------------------------------------------------------------------------

fn print_smoke_instructions() {
    println!("Smoke tests: follow `docs/SMOKE.md`.");
}

fn print_manual_instructions() {
    println!("Unable to determine how ao-rs was installed.");
    println!("Run whichever matches how you installed:");
    println!("- Homebrew: `brew upgrade ao-rs`  (confirm: `brew list --versions ao-rs`)");
    println!("- Cargo:    `cargo install ao-cli --locked`  (confirm: `which ao-rs` ends with `/.cargo/bin/ao-rs`)");
}

async fn run_or_explain(
    program: &str,
    args: &[&str],
    error_context: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    let status = Command::new(program).args(args).status().await;
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!(
            "{} (exit code {}).",
            error_context.unwrap_or("Command failed"),
            s.code().unwrap_or(-1)
        )
        .into()),
        Err(e) => Err(format!("{}: {}", error_context.unwrap_or("Command failed"), e).into()),
    }
}

// ---------------------------------------------------------------------------
// Latest-version resolvers
// ---------------------------------------------------------------------------

async fn resolve_latest_version() -> Result<Version, Box<dyn Error>> {
    // REST-first so we survive GitHub GraphQL rate limits; ao-ts parity.
    match GhRestResolver::new(REPO).latest_version().await {
        Ok(v) => Ok(v),
        Err(_) => GitTagsResolver::new(REPO_HTTPS)
            .latest_version()
            .await
            .map_err(|e| -> Box<dyn Error> { e }),
    }
}

#[async_trait]
trait LatestVersionResolver {
    async fn latest_version(&self) -> Result<Version, Box<dyn Error + Send + Sync>>;
}

struct GhRestResolver {
    repo: &'static str,
}

impl GhRestResolver {
    fn new(repo: &'static str) -> Self {
        Self { repo }
    }
}

#[async_trait]
impl LatestVersionResolver for GhRestResolver {
    async fn latest_version(&self) -> Result<Version, Box<dyn Error + Send + Sync>> {
        // REST: /repos/{owner}/{repo}/releases/latest → .tag_name
        // `gh api` counts against the REST budget (5000/h per user), not
        // the GraphQL budget used by `gh release view`.
        let path = format!("repos/{}/releases/latest", self.repo);
        let out = Command::new("gh")
            .args(["api", &path, "--jq", ".tag_name"])
            .output()
            .await?;

        if !out.status.success() {
            return Err(
                "failed to query latest release via `gh api` (is it installed and authenticated?)"
                    .into(),
            );
        }

        let raw = String::from_utf8(out.stdout)?;
        parse_version_tag(&raw).map_err(Into::into)
    }
}

fn parse_version_tag(input: &str) -> Result<Version, semver::Error> {
    let s = input.trim();
    let s = s.strip_prefix('v').unwrap_or(s);
    Version::parse(s)
}

struct GitTagsResolver {
    repo_url: &'static str,
}

impl GitTagsResolver {
    fn new(repo_url: &'static str) -> Self {
        Self { repo_url }
    }
}

#[async_trait]
impl LatestVersionResolver for GitTagsResolver {
    async fn latest_version(&self) -> Result<Version, Box<dyn Error + Send + Sync>> {
        let out = Command::new("git")
            .args(["ls-remote", "--tags", self.repo_url])
            .output()
            .await?;

        if !out.status.success() {
            return Err("failed to query tags via `git ls-remote` (is git installed?)".into());
        }

        let raw = String::from_utf8(out.stdout)?;
        latest_semver_from_ls_remote_tags(&raw)
            .ok_or_else(|| "no semver tags found in remote".into())
    }
}

fn latest_semver_from_ls_remote_tags(output: &str) -> Option<Version> {
    // Lines look like:
    // <sha>\trefs/tags/v1.2.3
    // <sha>\trefs/tags/v1.2.3^{}   (annotated tag deref; ignore)
    let mut best: Option<Version> = None;
    for line in output.lines() {
        let (_, r) = match line.split_once('\t') {
            Some(v) => v,
            None => continue,
        };
        if !r.starts_with("refs/tags/") || r.ends_with("^{}") {
            continue;
        }
        let tag = r.trim_start_matches("refs/tags/");
        let v = match parse_version_tag(tag) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if best.as_ref().is_none_or(|b| &v > b) {
            best = Some(v);
        }
    }
    best
}

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CacheData {
    latest_version: String,
    checked_at: String,
    current_version_at_check: String,
}

fn cache_path() -> PathBuf {
    cache_dir().join(CACHE_FILE_NAME)
}

fn cache_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("ao-rs");
        }
    }
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"));
    home.join(".cache").join("ao-rs")
}

fn read_cached_update_info(
    path: &Path,
    current_version: &str,
    now: SystemTime,
) -> Option<CacheData> {
    let raw = std::fs::read_to_string(path).ok()?;
    let data: CacheData = serde_json::from_str(&raw).ok()?;

    if data.latest_version.is_empty() || data.checked_at.is_empty() {
        return None;
    }
    if data.current_version_at_check != current_version {
        return None;
    }

    let checked_at = parse_rfc3339_utc(&data.checked_at)?;
    let age = now.duration_since(checked_at).ok()?;
    if age > CACHE_TTL {
        return None;
    }

    Some(data)
}

fn write_cache(path: &Path, data: &CacheData) {
    // Best-effort: a user-local cache failure must never crash the command.
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(json) = serde_json::to_string_pretty(data) else {
        return;
    };
    let _ = std::fs::write(path, json);
}

// ---------------------------------------------------------------------------
// RFC3339 (UTC, no fractional seconds) — dep-free round-trip for the cache.
// ---------------------------------------------------------------------------

fn format_rfc3339_utc(t: SystemTime) -> String {
    let secs = t
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs() as i64;
    let days = secs.div_euclid(86400);
    let tod = secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    let h = tod / 3600;
    let mn = (tod % 3600) / 60;
    let s = tod % 60;
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mn:02}:{s:02}Z")
}

fn parse_rfc3339_utc(s: &str) -> Option<SystemTime> {
    // Strict: only accepts our own `YYYY-MM-DDTHH:MM:SSZ` writer output.
    // We never need to consume arbitrary RFC3339 here.
    if s.len() != 20 || !s.ends_with('Z') {
        return None;
    }
    let b = s.as_bytes();
    if b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let year: i32 = s.get(0..4)?.parse().ok()?;
    let month: u32 = s.get(5..7)?.parse().ok()?;
    let day: u32 = s.get(8..10)?.parse().ok()?;
    let hour: u32 = s.get(11..13)?.parse().ok()?;
    let min: u32 = s.get(14..16)?.parse().ok()?;
    let sec: u32 = s.get(17..19)?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if hour > 23 || min > 59 || sec > 60 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let total = days
        .checked_mul(86400)?
        .checked_add(hour as i64 * 3600 + min as i64 * 60 + sec as i64)?;
    if total < 0 {
        return None;
    }
    Some(UNIX_EPOCH + Duration::from_secs(total as u64))
}

/// Howard Hinnant's civil→days (mirror of `ao-core::activity_log::days_from_civil`).
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y as i64 - 1 } else { y as i64 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m_adj = if m > 2 { m as i64 - 3 } else { m as i64 + 9 };
    let doy = (153 * m_adj + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Howard Hinnant's days→civil. Inverse of `days_from_civil`.
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parse_version_tag_strips_leading_v() {
        let v = parse_version_tag("v1.2.3\n").unwrap();
        assert_eq!(v, Version::new(1, 2, 3));
    }

    #[test]
    fn parse_version_tag_accepts_plain_semver() {
        let v = parse_version_tag("0.0.1").unwrap();
        assert_eq!(v, Version::new(0, 0, 1));
    }

    struct FakeResolver(Version);

    #[async_trait]
    impl LatestVersionResolver for FakeResolver {
        async fn latest_version(&self) -> Result<Version, Box<dyn Error + Send + Sync>> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn resolver_trait_can_be_mocked() {
        let v = FakeResolver(Version::new(9, 9, 9))
            .latest_version()
            .await
            .unwrap();
        assert_eq!(v, Version::new(9, 9, 9));
    }

    #[test]
    fn latest_semver_from_ls_remote_tags_picks_highest_and_ignores_deref() {
        let out = "\
aaaaaaaa\trefs/tags/v0.1.0\n\
bbbbbbbb\trefs/tags/v0.2.0\n\
cccccccc\trefs/tags/v0.2.0^{}\n\
dddddddd\trefs/tags/not-a-version\n\
";
        let v = latest_semver_from_ls_remote_tags(out).unwrap();
        assert_eq!(v, Version::new(0, 2, 0));
    }

    #[test]
    fn recommended_command_covers_every_method() {
        assert_eq!(
            recommended_command(InstallMethod::Homebrew),
            "brew upgrade ao-rs"
        );
        assert_eq!(
            recommended_command(InstallMethod::Cargo),
            "cargo install ao-cli --locked"
        );
        let unknown = recommended_command(InstallMethod::Unknown);
        assert!(unknown.contains("brew upgrade ao-rs"));
        assert!(unknown.contains("cargo install ao-cli --locked"));
    }

    #[test]
    fn install_method_serializes_as_lowercase() {
        assert_eq!(
            serde_json::to_string(&InstallMethod::Homebrew).unwrap(),
            "\"homebrew\""
        );
        assert_eq!(
            serde_json::to_string(&InstallMethod::Cargo).unwrap(),
            "\"cargo\""
        );
        assert_eq!(
            serde_json::to_string(&InstallMethod::Unknown).unwrap(),
            "\"unknown\""
        );
    }

    #[test]
    fn is_cargo_bin_matches_default_cargo_layout() {
        assert!(is_cargo_bin(Path::new("/Users/alice/.cargo/bin/ao-rs")));
        assert!(is_cargo_bin(Path::new("/home/bob/.cargo/bin/ao-rs")));
    }

    #[test]
    fn is_cargo_bin_rejects_unrelated_paths() {
        assert!(!is_cargo_bin(Path::new("/usr/local/bin/ao-rs")));
        assert!(!is_cargo_bin(Path::new("/opt/homebrew/bin/ao-rs")));
    }

    #[test]
    fn is_cargo_bin_respects_cargo_home_env() {
        // Save and restore to avoid poisoning other tests.
        let prev = std::env::var("CARGO_HOME").ok();
        std::env::set_var("CARGO_HOME", "/opt/cargo");
        assert!(is_cargo_bin(Path::new("/opt/cargo/bin/ao-rs")));
        assert!(!is_cargo_bin(Path::new("/opt/rust/bin/ao-rs")));
        match prev {
            Some(v) => std::env::set_var("CARGO_HOME", v),
            None => std::env::remove_var("CARGO_HOME"),
        }
    }

    #[test]
    fn rfc3339_format_parse_roundtrips() {
        let t = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let s = format_rfc3339_utc(t);
        assert_eq!(s, "2023-11-14T22:13:20Z");
        let back = parse_rfc3339_utc(&s).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn rfc3339_parse_rejects_non_utc_suffix() {
        assert!(parse_rfc3339_utc("2023-11-14T22:13:20+01:00").is_none());
        assert!(parse_rfc3339_utc("2023-11-14T22:13:20").is_none());
        assert!(parse_rfc3339_utc("not-a-date").is_none());
    }

    #[test]
    fn rfc3339_parse_rejects_fractional_seconds() {
        // Our cache only ever emits whole seconds; keeping the parser strict
        // catches any future divergence on the writer side.
        assert!(parse_rfc3339_utc("2023-11-14T22:13:20.5Z").is_none());
    }

    fn write_cache_file(dir: &Path, data: &CacheData) -> PathBuf {
        let path = dir.join("update-check.json");
        let json = serde_json::to_string_pretty(data).unwrap();
        std::fs::write(&path, json).unwrap();
        path
    }

    #[test]
    fn read_cache_returns_data_when_fresh() {
        let dir = tempdir().unwrap();
        let now = UNIX_EPOCH + Duration::from_secs(2_000_000_000);
        let data = CacheData {
            latest_version: "0.0.2".into(),
            checked_at: format_rfc3339_utc(now - Duration::from_secs(3600)),
            current_version_at_check: "0.0.1".into(),
        };
        let path = write_cache_file(dir.path(), &data);
        let got = read_cached_update_info(&path, "0.0.1", now).unwrap();
        assert_eq!(got.latest_version, "0.0.2");
    }

    #[test]
    fn read_cache_returns_none_past_ttl() {
        let dir = tempdir().unwrap();
        let now = UNIX_EPOCH + Duration::from_secs(2_000_000_000);
        let data = CacheData {
            latest_version: "0.0.2".into(),
            checked_at: format_rfc3339_utc(now - CACHE_TTL - Duration::from_secs(1)),
            current_version_at_check: "0.0.1".into(),
        };
        let path = write_cache_file(dir.path(), &data);
        assert!(read_cached_update_info(&path, "0.0.1", now).is_none());
    }

    #[test]
    fn read_cache_returns_none_when_current_version_changed() {
        let dir = tempdir().unwrap();
        let now = UNIX_EPOCH + Duration::from_secs(2_000_000_000);
        let data = CacheData {
            latest_version: "0.0.2".into(),
            checked_at: format_rfc3339_utc(now),
            current_version_at_check: "0.0.1".into(),
        };
        let path = write_cache_file(dir.path(), &data);
        assert!(read_cached_update_info(&path, "0.0.2", now).is_none());
    }

    #[test]
    fn read_cache_returns_none_for_missing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nope.json");
        assert!(read_cached_update_info(&path, "0.0.1", SystemTime::now()).is_none());
    }

    #[test]
    fn read_cache_returns_none_for_malformed_json() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(CACHE_FILE_NAME);
        std::fs::write(&path, "not json").unwrap();
        assert!(read_cached_update_info(&path, "0.0.1", SystemTime::now()).is_none());
    }

    #[test]
    fn write_cache_is_idempotent_and_round_trips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested/dir/update-check.json");
        let data = CacheData {
            latest_version: "0.0.2".into(),
            checked_at: format_rfc3339_utc(UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
            current_version_at_check: "0.0.1".into(),
        };
        write_cache(&path, &data);
        write_cache(&path, &data); // second call must not panic.
        let got = read_cached_update_info(
            &path,
            "0.0.1",
            UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        )
        .unwrap();
        assert_eq!(got.latest_version, "0.0.2");
    }

    #[test]
    fn update_info_serializes_as_camel_case() {
        let info = UpdateInfo {
            current_version: "0.0.1".into(),
            latest_version: Some("0.0.2".into()),
            is_outdated: true,
            install_method: InstallMethod::Cargo,
            recommended_command: "cargo install ao-cli --locked".into(),
            checked_at: Some("2026-04-18T00:00:00Z".into()),
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("\"currentVersion\""));
        assert!(json.contains("\"latestVersion\""));
        assert!(json.contains("\"isOutdated\""));
        assert!(json.contains("\"installMethod\":\"cargo\""));
        assert!(json.contains("\"recommendedCommand\""));
        assert!(json.contains("\"checkedAt\""));
    }

    #[test]
    fn update_info_null_latest_version_when_resolver_fails() {
        let info = UpdateInfo {
            current_version: "0.0.1".into(),
            latest_version: None,
            is_outdated: false,
            install_method: InstallMethod::Unknown,
            recommended_command: recommended_command(InstallMethod::Unknown),
            checked_at: None,
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("\"latestVersion\":null"));
        assert!(json.contains("\"checkedAt\":null"));
        assert!(json.contains("\"isOutdated\":false"));
    }

    #[test]
    fn compare_outdated_handles_invalid_latest() {
        let v = Version::parse("0.0.1").unwrap();
        assert!(!compare_outdated(&v, ""));
        assert!(!compare_outdated(&v, "not-a-version"));
        assert!(compare_outdated(&v, "0.0.2"));
        assert!(!compare_outdated(&v, "0.0.0"));
        assert!(!compare_outdated(&v, "0.0.1"));
    }

    #[test]
    fn cache_dir_prefers_xdg_cache_home() {
        let prev_xdg = std::env::var("XDG_CACHE_HOME").ok();
        std::env::set_var("XDG_CACHE_HOME", "/tmp/xdg-cache");
        let p = cache_dir();
        assert_eq!(p, PathBuf::from("/tmp/xdg-cache/ao-rs"));
        match prev_xdg {
            Some(v) => std::env::set_var("XDG_CACHE_HOME", v),
            None => std::env::remove_var("XDG_CACHE_HOME"),
        }
    }

    #[test]
    fn cache_dir_falls_back_to_home_dot_cache_when_xdg_unset() {
        let prev_xdg = std::env::var("XDG_CACHE_HOME").ok();
        let prev_home = std::env::var("HOME").ok();
        std::env::remove_var("XDG_CACHE_HOME");
        std::env::set_var("HOME", "/tmp/home-alice");
        let p = cache_dir();
        assert_eq!(p, PathBuf::from("/tmp/home-alice/.cache/ao-rs"));
        match prev_xdg {
            Some(v) => std::env::set_var("XDG_CACHE_HOME", v),
            None => std::env::remove_var("XDG_CACHE_HOME"),
        }
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}
