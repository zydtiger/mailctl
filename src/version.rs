use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct VersionInfo {
    pub name: &'static str,
    pub version: &'static str,
    pub git_commit: Option<&'static str>,
    pub target: &'static str,
    pub profile: &'static str,
}

impl VersionInfo {
    pub fn human(&self) -> String {
        let commit = self
            .git_commit
            .map(|commit| format!(" ({})", &commit[..commit.len().min(12)]))
            .unwrap_or_default();
        format!(
            "{} {}{commit} {} {}",
            self.name, self.version, self.target, self.profile
        )
    }
}

pub fn info() -> VersionInfo {
    VersionInfo {
        name: env!("CARGO_PKG_NAME"),
        version: env!("CARGO_PKG_VERSION"),
        git_commit: option_env!("MAILCTL_GIT_COMMIT"),
        target: env!("MAILCTL_BUILD_TARGET"),
        profile: env!("MAILCTL_BUILD_PROFILE"),
    }
}

#[cfg(test)]
mod tests {
    const INFO_PLIST: &str = include_str!("../Info.plist");

    #[test]
    fn info_plist_version_matches_cargo() {
        for key in ["CFBundleShortVersionString", "CFBundleVersion"] {
            let value = INFO_PLIST
                .split_once(&format!("<key>{key}</key>"))
                .and_then(|(_, rest)| rest.split_once("<string>"))
                .and_then(|(_, rest)| rest.split_once("</string>"))
                .map(|(value, _)| value);
            assert_eq!(value, Some(env!("CARGO_PKG_VERSION")), "{key}");
        }
    }
}
