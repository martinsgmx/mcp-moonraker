//! Safety layer: policy decisions, double-confirmation approvals, redaction.

pub mod approval;
pub mod policy;
pub mod redact;

pub use approval::{ApprovalPrompt, ApprovalRecord, ApprovalStatus, ApprovalStore};
pub use policy::{Capability, Decision, Policy, Risk};
