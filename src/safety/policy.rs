//! Local policy engine.
//!
//! The policy is intentionally explicit: every tool declares a [`Capability`]
//! and a [`Risk`]. The policy decides whether the action is allowed, needs a
//! double confirmation, or is denied outright. No tool may bypass this layer
//! (except the dedicated emergency-stop path, which is a safety action).

use serde::{Deserialize, Serialize};

use crate::config::PolicyConfig;

/// What class of effect a tool has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Pure read.
    Read,
    /// Changes printer state but is not destructive (heaters, fans, motion,
    /// speed/flow, pause/resume, queue additions).
    Mutation,
    /// Host administration: service restart, reboot, shutdown, updates.
    HostAdmin,
    /// Arbitrary G-code execution.
    RawGcode,
    /// Configuration-file writes.
    ConfigWrite,
}

impl Capability {
    /// Stable label used in audit records and tool metadata.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Mutation => "mutation",
            Self::HostAdmin => "host_admin",
            Self::RawGcode => "raw_gcode",
            Self::ConfigWrite => "config_write",
        }
    }
}

/// How dangerous an action is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    /// Read-only.
    Read,
    /// Reversible, low-impact mutation.
    Low,
    /// Reversible but potentially surprising mutation.
    Medium,
    /// Destructive or disruptive: cancels prints, deletes files, restarts.
    High,
    /// Affects the host, arbitrary G-code, or config files.
    Critical,
}

impl Risk {
    /// Human label used in approval prompts and audit records.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }
}

/// Result of a policy evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Execute immediately.
    Allow,
    /// Create an approval request and do not execute yet.
    NeedsApproval,
    /// Refuse with a human-readable reason.
    Deny(String),
}

/// Policy evaluator.
#[derive(Debug, Clone)]
pub struct Policy {
    config: PolicyConfig,
}

impl Policy {
    /// Build a policy from configuration.
    #[must_use]
    pub fn new(config: PolicyConfig) -> Self {
        Self { config }
    }

    /// Access the underlying configuration.
    #[must_use]
    pub fn config(&self) -> &PolicyConfig {
        &self.config
    }

    /// Evaluate a capability/risk pair.
    #[must_use]
    pub fn evaluate(&self, capability: Capability, risk: Risk) -> Decision {
        if capability == Capability::Read || risk == Risk::Read {
            return Decision::Allow;
        }

        if self.config.read_only {
            return Decision::Deny(
                "server is in read-only mode (MCP_MOONRAKER_READ_ONLY=true)".to_string(),
            );
        }

        if !self.config.allow_mutations {
            return Decision::Deny(
                "mutations are disabled (MCP_MOONRAKER_ALLOW_MUTATIONS=false)".to_string(),
            );
        }

        match capability {
            Capability::Read => Decision::Allow,
            Capability::Mutation => {
                if risk >= Risk::High {
                    Decision::NeedsApproval
                } else {
                    Decision::Allow
                }
            }
            Capability::HostAdmin => {
                if self.config.allow_host_admin {
                    Decision::NeedsApproval
                } else {
                    Decision::Deny(
                        "host administration is disabled \
                         (MCP_MOONRAKER_ALLOW_HOST_ADMIN=false)"
                            .to_string(),
                    )
                }
            }
            Capability::RawGcode => {
                if self.config.allow_raw_gcode {
                    Decision::NeedsApproval
                } else {
                    Decision::Deny(
                        "raw G-code is disabled (MCP_MOONRAKER_ALLOW_RAW_GCODE=false)".to_string(),
                    )
                }
            }
            Capability::ConfigWrite => {
                if self.config.allow_config_write {
                    Decision::NeedsApproval
                } else {
                    Decision::Deny(
                        "configuration writes are disabled \
                         (MCP_MOONRAKER_ALLOW_CONFIG_WRITE=false)"
                            .to_string(),
                    )
                }
            }
        }
    }

    /// Emergency stop is a safety action and bypasses `read_only`.
    #[must_use]
    pub fn emergency_stop_allowed(&self) -> bool {
        !self.config.disable_emergency_stop
    }

    /// Maximum target for a heater.
    #[must_use]
    pub fn max_temp_for(&self, heater: &str) -> f64 {
        let lower = heater.to_ascii_lowercase();
        if lower.contains("bed") {
            self.config.max_bed_temp
        } else {
            self.config.max_hotend_temp
        }
    }

    /// Secret-free policy summary for the `policy_info` tool.
    #[must_use]
    pub fn summary(&self) -> serde_json::Value {
        serde_json::json!({
            "read_only": self.config.read_only,
            "allow_mutations": self.config.allow_mutations,
            "allow_raw_gcode": self.config.allow_raw_gcode,
            "allow_host_admin": self.config.allow_host_admin,
            "allow_config_write": self.config.allow_config_write,
            "disable_emergency_stop": self.config.disable_emergency_stop,
            "max_hotend_temp": self.config.max_hotend_temp,
            "max_bed_temp": self.config.max_bed_temp,
            "approval_required_for": ["high", "critical"],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> Policy {
        Policy::new(PolicyConfig::default())
    }

    #[test]
    fn reads_are_always_allowed() {
        assert_eq!(
            policy().evaluate(Capability::Read, Risk::Read),
            Decision::Allow
        );
    }

    #[test]
    fn destructive_needs_approval() {
        assert_eq!(
            policy().evaluate(Capability::Mutation, Risk::High),
            Decision::NeedsApproval
        );
        assert_eq!(
            policy().evaluate(Capability::Mutation, Risk::Medium),
            Decision::Allow
        );
    }

    #[test]
    fn raw_gcode_and_admin_are_denied_by_default() {
        assert!(matches!(
            policy().evaluate(Capability::RawGcode, Risk::Critical),
            Decision::Deny(_)
        ));
        assert!(matches!(
            policy().evaluate(Capability::HostAdmin, Risk::Critical),
            Decision::Deny(_)
        ));
    }

    #[test]
    fn read_only_denies_mutations() {
        let config = PolicyConfig {
            read_only: true,
            ..PolicyConfig::default()
        };
        let policy = Policy::new(config);
        assert!(matches!(
            policy.evaluate(Capability::Mutation, Risk::Low),
            Decision::Deny(_)
        ));
        assert!(policy.emergency_stop_allowed());
    }

    #[test]
    fn bed_uses_bed_limit() {
        let policy = policy();
        assert!((policy.max_temp_for("heater_bed") - 120.0).abs() < f64::EPSILON);
        assert!((policy.max_temp_for("extruder") - 300.0).abs() < f64::EPSILON);
        assert!((policy.max_temp_for("heater_generic chamber") - 300.0).abs() < f64::EPSILON);
    }
}
