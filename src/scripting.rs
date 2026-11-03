//! Runs the embedded JXA bridge through `osascript`.
//!
//! `osascript` inherits mailctl's TCC responsibility, so the Automation
//! prompt for Mail names mailctl rather than the app that launched it.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::process::Command;

const SCRIPT: &str = include_str!("jxa/mail.js");

#[derive(Deserialize)]
struct Response {
    ok: bool,
    #[serde(default)]
    value: Value,
    #[serde(default)]
    error: String,
}

/// Sends one request to Mail.app and decodes the handler's return value.
pub fn call<T: DeserializeOwned>(request: &Value) -> Result<T> {
    let output = Command::new("/usr/bin/osascript")
        .args(["-l", "JavaScript", "-e", SCRIPT, &request.to_string()])
        .output()
        .context("failed to run osascript")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "Mail scripting failed: {}{}",
            stderr.trim(),
            permission_hint(&stderr)
        );
    }
    let response: Response =
        serde_json::from_slice(&output.stdout).context("Mail scripting returned invalid JSON")?;
    if !response.ok {
        bail!(
            "Mail scripting failed: {}{}",
            response.error,
            permission_hint(&response.error)
        );
    }
    serde_json::from_value(response.value).context("unexpected Mail scripting result")
}

fn permission_hint(message: &str) -> &'static str {
    if message.contains("-1743") || message.contains("Not authorized") {
        "\nmailctl needs Automation permission for Mail; run `mailctl permissions`."
    } else {
        ""
    }
}
