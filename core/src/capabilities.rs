//! Human-approved Implementer execution capabilities embedded in a Plan.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LocalServerCapability {
    None,
    Loopback,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BrowserCapability {
    None,
    Headless,
    Headed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionCapabilities {
    pub shell: bool,
    pub internet: bool,
    pub local_server: LocalServerCapability,
    pub browser: BrowserCapability,
    pub artifacts: bool,
    pub timeout_minutes: u64,
}

impl ExecutionCapabilities {
    pub fn parse_plan(plan: &str) -> Result<ExecutionCapabilities, CapabilityError> {
        let section = execution_section(plan).ok_or(CapabilityError::MissingSection)?;
        let yaml = fenced_yaml(section).ok_or(CapabilityError::MissingYamlFence)?;
        let capabilities: ExecutionCapabilities = serde_yaml::from_str(yaml)?;
        capabilities.validate()?;
        Ok(capabilities)
    }

    pub fn timeout_seconds(&self) -> u64 {
        self.timeout_minutes.saturating_mul(60)
    }

    fn validate(&self) -> Result<(), CapabilityError> {
        if self.timeout_minutes == 0 {
            return Err(CapabilityError::Invalid(
                "timeout_minutes must be at least 1".to_string(),
            ));
        }
        if self.local_server == LocalServerCapability::Loopback && !self.shell {
            return Err(CapabilityError::Invalid(
                "local_server: loopback requires shell: true".to_string(),
            ));
        }
        if self.browser != BrowserCapability::None && !self.artifacts {
            return Err(CapabilityError::Invalid(
                "browser access requires artifacts: true".to_string(),
            ));
        }
        if self.browser != BrowserCapability::None && !self.internet {
            return Err(CapabilityError::Invalid(
                "browser access requires internet: true because the browser sidecar runs outside the network sandbox"
                    .to_string(),
            ));
        }
        Ok(())
    }
}

impl std::fmt::Display for ExecutionCapabilities {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&serde_yaml::to_string(self).map_err(|_| std::fmt::Error)?)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CapabilityError {
    #[error("Plan is missing an Execution capabilities section")]
    MissingSection,
    #[error("Execution capabilities must be an exhaustive fenced YAML document")]
    MissingYamlFence,
    #[error("invalid Execution capabilities YAML: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("invalid Execution capabilities: {0}")]
    Invalid(String),
}

fn execution_section(plan: &str) -> Option<&str> {
    let mut offset = 0;
    for line in plan.split_inclusive('\n') {
        let heading = line.trim();
        if heading == "### Execution capabilities" || heading == "## Execution capabilities" {
            let start = offset + line.len();
            let rest = &plan[start..];
            let end = rest
                .match_indices("\n##")
                .find(|(_, suffix)| suffix.starts_with("\n## ") || suffix.starts_with("\n### "))
                .map(|(index, _)| index)
                .unwrap_or(rest.len());
            return Some(&rest[..end]);
        }
        offset += line.len();
    }
    None
}

fn fenced_yaml(section: &str) -> Option<&str> {
    let start_marker = "```yaml";
    let start = section.find(start_marker)? + start_marker.len();
    let body = &section[start..];
    let end = body.find("```")?;
    Some(body[..end].trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAN: &str = r#"
### Summary
Build and validate the application.

### Execution capabilities
```yaml
shell: true
internet: true
local_server: loopback
browser: headed
artifacts: true
timeout_minutes: 30
```

### Steps
1. Implement.
"#;

    #[test]
    fn parses_exhaustive_plan_capabilities() {
        let capabilities = ExecutionCapabilities::parse_plan(PLAN).unwrap();
        assert!(capabilities.shell);
        assert!(capabilities.internet);
        assert_eq!(capabilities.local_server, LocalServerCapability::Loopback);
        assert_eq!(capabilities.browser, BrowserCapability::Headed);
        assert_eq!(capabilities.timeout_seconds(), 1800);
    }

    #[test]
    fn rejects_missing_or_partial_capabilities() {
        assert!(matches!(
            ExecutionCapabilities::parse_plan("### Summary\nNo grant"),
            Err(CapabilityError::MissingSection)
        ));
        let partial = "### Execution capabilities\n```yaml\nshell: true\n```";
        assert!(matches!(
            ExecutionCapabilities::parse_plan(partial),
            Err(CapabilityError::Yaml(_))
        ));
    }

    #[test]
    fn validates_capability_dependencies() {
        let invalid_server = PLAN.replace("shell: true", "shell: false");
        assert!(matches!(
            ExecutionCapabilities::parse_plan(&invalid_server),
            Err(CapabilityError::Invalid(_))
        ));
        let invalid_browser = PLAN.replace("internet: true", "internet: false");
        assert!(matches!(
            ExecutionCapabilities::parse_plan(&invalid_browser),
            Err(CapabilityError::Invalid(_))
        ));
    }
}
