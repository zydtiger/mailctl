use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=MAILCTL_GIT_COMMIT");
    println!("cargo:rerun-if-changed=.git/HEAD");
    watch_checked_out_ref();

    if let Some(commit) = env::var("MAILCTL_GIT_COMMIT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(git_commit)
    {
        println!("cargo:rustc-env=MAILCTL_GIT_COMMIT={commit}");
    }
    println!(
        "cargo:rustc-env=MAILCTL_BUILD_TARGET={}",
        env::var("TARGET").unwrap_or_else(|_| "unknown".to_string())
    );
    println!(
        "cargo:rustc-env=MAILCTL_BUILD_PROFILE={}",
        env::var("PROFILE").unwrap_or_else(|_| "unknown".to_string())
    );
}

fn git_commit() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| !value.is_empty())
}

fn watch_checked_out_ref() {
    let Ok(head) = fs::read_to_string(".git/HEAD") else {
        return;
    };
    let Some(reference) = head.trim().strip_prefix("ref: ") else {
        return;
    };
    let path = Path::new(".git").join(reference);
    if path.exists() {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}
