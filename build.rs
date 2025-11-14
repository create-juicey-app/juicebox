use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const UNKNOWN: &str = "unknown";

const BRANCH_ENV_KEYS: &[&str] = &[
    "JUICEBOX_BUILD_BRANCH",
    "GIT_BRANCH",
    "GITHUB_REF_NAME",
    "BRANCH_NAME",
];

const FULL_COMMIT_ENV_KEYS: &[&str] = &[
    "JUICEBOX_BUILD_COMMIT",
    "GIT_COMMIT",
    "GIT_SHA",
    "GITHUB_SHA",
    "VERCEL_GIT_COMMIT_SHA",
    "COMMIT_SHA",
    "SOURCE_VERSION",
];

const SHORT_COMMIT_ENV_KEYS: &[&str] = &[
    "JUICEBOX_BUILD_COMMIT_SHORT",
    "GIT_COMMIT_SHORT",
    "GIT_SHA_SHORT",
    "GITHUB_SHA_SHORT",
    "VERCEL_GIT_COMMIT_SHA_SHORT",
    "COMMIT_SHA_SHORT",
];

fn main() {
    emit_rerun_for_git();
    let branch = resolve_branch();
    let commit_full = resolve_commit_full();
    let commit_short = resolve_commit_short(&commit_full);
    println!("cargo:rustc-env=JUICEBOX_GIT_BRANCH={branch}");
    println!("cargo:rustc-env=JUICEBOX_GIT_COMMIT={commit_full}");
    println!("cargo:rustc-env=JUICEBOX_GIT_COMMIT_SHORT={commit_short}");
}

fn resolve_branch() -> String {
    if let Some(value) = env_value(BRANCH_ENV_KEYS) {
        return value;
    }
    if let Some(value) = run_git(&["rev-parse", "--abbrev-ref", "HEAD"]) {
        if value != "HEAD" {
            return value;
        }
    }
    env_value(&["CI_COMMIT_REF_NAME"]).unwrap_or_else(|| UNKNOWN.to_string())
}

fn resolve_commit_full() -> String {
    if let Some(value) = env_value(FULL_COMMIT_ENV_KEYS) {
        return value;
    }
    if let Some(value) = run_git(&["rev-parse", "HEAD"]) {
        return value;
    }
    if let Some(value) = env_value(SHORT_COMMIT_ENV_KEYS) {
        return value;
    }
    UNKNOWN.to_string()
}

fn resolve_commit_short(full_commit: &str) -> String {
    if let Some(value) = env_value(SHORT_COMMIT_ENV_KEYS) {
        return value;
    }
    if let Some(value) = run_git(&["rev-parse", "--short", "HEAD"]) {
        return value;
    }
    short_commit(full_commit)
}

fn short_commit(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed == UNKNOWN {
        return UNKNOWN.to_string();
    }
    let short: String = trimmed.chars().take(12).collect();
    if short.is_empty() {
        UNKNOWN.to_string()
    } else {
        short
    }
}

fn env_value(keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|key| env::var(key).ok())
        .map(|value| value.trim().to_string())
        .find(|value| !value.is_empty())
}

fn run_git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn emit_rerun_for_git() {
    for key in BRANCH_ENV_KEYS
        .iter()
        .chain(FULL_COMMIT_ENV_KEYS.iter())
        .chain(SHORT_COMMIT_ENV_KEYS.iter())
    {
        println!("cargo:rerun-if-env-changed={key}");
    }
    println!("cargo:rerun-if-env-changed=CI_COMMIT_REF_NAME");

    let head_path = Path::new(".git/HEAD");
    println!("cargo:rerun-if-changed={}", head_path.display());
    if let Some(reference) = head_reference_path(head_path) {
        println!("cargo:rerun-if-changed={}", reference.display());
    }
}

fn head_reference_path(head_path: &Path) -> Option<PathBuf> {
    let head = fs::read_to_string(head_path).ok()?;
    let reference = head.strip_prefix("ref:")?.trim();
    if reference.is_empty() {
        None
    } else {
        Some(PathBuf::from(".git").join(reference))
    }
}
