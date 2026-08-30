#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Platform {
    Unix,
    Windows,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformCapabilities {
    pub platform: Platform,
    pub symlink: Capability,
    pub junction: Capability,
    pub process_sandbox: Capability,
    pub limitations: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Capability {
    Supported,
    Unsupported,
}

impl PlatformCapabilities {
    pub fn for_platform(platform: Platform) -> Self {
        match platform {
            Platform::Unix => Self {
                platform,
                symlink: Capability::Supported,
                junction: Capability::Unsupported,
                process_sandbox: Capability::Unsupported,
                limitations: vec![
                    "This crate plans links only; it does not enforce process sandboxing.".into(),
                ],
            },
            Platform::Windows => Self {
                platform,
                symlink: Capability::Supported,
                junction: Capability::Supported,
                process_sandbox: Capability::Unsupported,
                limitations: vec![
                    "Link mutation and process sandboxing are outside this planning crate.".into(),
                ],
            },
            Platform::Other => Self {
                platform,
                symlink: Capability::Unsupported,
                junction: Capability::Unsupported,
                process_sandbox: Capability::Unsupported,
                limitations: vec![
                    "This platform has no supported link strategy in this release.".into(),
                    "Process sandboxing is not provided.".into(),
                ],
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_platform_reports_limitations() {
        let caps = PlatformCapabilities::for_platform(Platform::Other);
        assert_eq!(caps.symlink, Capability::Unsupported);
        assert_eq!(caps.process_sandbox, Capability::Unsupported);
        assert!(!caps.limitations.is_empty());
    }
}
