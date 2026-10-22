//! What a write asks of a child and the body of the guardian route (spec 3.4.1, 4.9).

use serde::Deserialize;

use crate::people::{ChildStatus, GuardianLink};

/// What a write asks of `is_child` and `child_status`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChildChange {
    pub is_child: Option<bool>,
    pub child_status: Option<ChildStatus>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GuardianRequest {
    pub(super) guardian_id: String,
    pub(super) link: GuardianLink,
}
