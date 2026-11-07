//! Runs the embedded JXA bridges through `osascript`: `mail.js` for Mail and
//! `outlook.js` for Outlook meetings.
//!
//! `osascript` inherits mailctl's TCC responsibility, so the Automation
//! prompt for Mail and the Accessibility check for Outlook name mailctl
//! rather than the app that launched it.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::process::Command;

const MAIL: &str = include_str!("jxa/mail.js");
const OUTLOOK: &str = include_str!("jxa/outlook.js");

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
    run(MAIL, "Mail", request)
}

/// Sends one request to Microsoft Outlook and decodes the handler's return value.
pub fn outlook<T: DeserializeOwned>(request: &Value) -> Result<T> {
    run(OUTLOOK, "Outlook", request)
}

fn run<T: DeserializeOwned>(script: &str, app: &str, request: &Value) -> Result<T> {
    let output = Command::new("/usr/bin/osascript")
        .args(["-l", "JavaScript", "-e", script, &request.to_string()])
        .output()
        .context("failed to run osascript")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "{app} scripting failed: {}{}",
            stderr.trim(),
            permission_hint(&stderr)
        );
    }
    let response: Response = serde_json::from_slice(&output.stdout)
        .with_context(|| format!("{app} scripting returned invalid JSON"))?;
    if !response.ok {
        bail!(
            "{app} scripting failed: {}{}",
            response.error,
            permission_hint(&response.error)
        );
    }
    serde_json::from_value(response.value)
        .with_context(|| format!("unexpected {app} scripting result"))
}

fn permission_hint(message: &str) -> &'static str {
    if message.contains("-1743") || message.contains("Not authorized") {
        "\nmailctl needs Automation permission for Mail; run `mailctl permissions`."
    } else if message.contains("Accessibility permission") || message.contains("-25211") {
        "\nRun `mailctl permissions --check` for the binary to add under Accessibility."
    } else {
        ""
    }
}
