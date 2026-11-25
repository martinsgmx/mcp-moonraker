//! Safety layer: policy decisions, double-confirmation approvals, redaction.

pub mod approval;
pub mod policy;
pub mod redact;

pub use approval::{
    target_from_args, ApprovalFailure, ApprovalPrompt, ApprovalRecord, ApprovalRejection,
    ApprovalStatus, ApprovalStore,
};
pub use policy::{Capability, Decision, Policy, Risk};
