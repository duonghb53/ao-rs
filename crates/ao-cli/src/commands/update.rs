use std::error::Error;
use std::path::PathBuf;

use async_trait::async_trait;
use semver::Version;
use tokio::process::Command;

const REPO: &str = "duonghb53/ao-rs";

pub async fn update(check: bool, skip_smoke: bool, smoke_only: bool) -> Result<(), Box<dyn Error>> {
    if smoke_only {
        print_smoke_instructions();
        return Ok(());
    }

    let current = Version::parse(env!("CARGO_PKG_VERSION"))?;
    let latest = resolve_latest_version(&GitHubGhResolver::new(REPO)).await?;

    if check {
        print_check(&current, &latest);
        return Ok(());
    }

    if latest <= current {
        println!("ao-rs is up to date ({}).", current);
        return Ok(());
    }

    println!("Updating ao-rs: {} -> {}.", current, latest);

    let exe = std::env::current_exe().ok();
    let method = detect_install_method(exe.as_ref()).await;

    match method {
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

fn print_check(current: &Version, latest: &Version) {
    if latest > current {
        println!(
            "Update available: current {} < latest {}. Run `ao-rs update` to upgrade.",
            current, latest
        );
    } else {
        println!("Up to date: {}.", current);
    }
}

fn print_smoke_instructions() {
    println!("Smoke tests: follow `docs/SMOKE.md`.");
}

fn print_manual_instructions() {
    println!("Unable to determine how ao-rs was installed.");
    println!("Try one of:");
    println!("- Cargo: `cargo install ao-cli --locked`");
    println!("- Homebrew: `brew upgrade ao-rs`");
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallMethod {
    Homebrew,
    Cargo,
    Unknown,
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
    // Heuristic only; avoids platform-specific cargo metadata probing.
    p.to_string_lossy().contains("/.cargo/bin/")
}

async fn resolve_latest_version(
    resolver: &dyn LatestVersionResolver,
) -> Result<Version, Box<dyn Error>> {
    resolver
        .latest_version()
        .await
        .map_err(|e| -> Box<dyn Error> { e })
}

#[async_trait]
trait LatestVersionResolver {
    async fn latest_version(&self) -> Result<Version, Box<dyn Error + Send + Sync>>;
}

struct GitHubGhResolver {
    repo: &'static str,
}

impl GitHubGhResolver {
    fn new(repo: &'static str) -> Self {
        Self { repo }
    }
}

#[async_trait]
impl LatestVersionResolver for GitHubGhResolver {
    async fn latest_version(&self) -> Result<Version, Box<dyn Error + Send + Sync>> {
        let out = Command::new("gh")
            .args([
                "release", "view", "--repo", self.repo, "--json", "tagName", "--jq", ".tagName",
            ])
            .output()
            .await?;

        if !out.status.success() {
            return Err(
                "failed to query latest release via `gh` (is it installed and authenticated?)"
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

#[cfg(test)]
mod tests {
    use super::*;

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
    async fn resolve_latest_version_uses_resolver() {
        let v = resolve_latest_version(&FakeResolver(Version::new(9, 9, 9)))
            .await
            .unwrap();
        assert_eq!(v, Version::new(9, 9, 9));
    }
}
