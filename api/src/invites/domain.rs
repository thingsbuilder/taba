//! The rules on an invite (spec 3.3): when it is valid, who it admits, what it grants and how it
//! is spent. Nothing here is `async` or reads the database.

use axum::http::StatusCode;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use firestore::FirestoreInstant;
use firestore::jiff::SignedDuration;

use super::model::{InviteDoc, InviteKind, InviteStatus, invite_invalid};
use crate::error::ApiError;
use crate::members::{MembershipDoc, MembershipStatus, Via};

pub(super) const QR_TTL: SignedDuration = SignedDuration::from_mins(30);
pub(super) const LINK_TTL: SignedDuration = SignedDuration::from_hours(24);

impl InviteDoc {
    /// Whether the invite can still be accepted: not used, not canceled, not expired.
    pub fn is_valid(&self, now: FirestoreInstant) -> bool {
        self.status == InviteStatus::Active && now < self.expires_at
    }

    pub(super) fn via(&self) -> Via {
        match self.kind {
            InviteKind::Qr => Via::Qr,
            InviteKind::Link => Via::Link,
        }
    }

    /// Spends the invite on `person_id`. Single use: only a valid invite can be spent, so a replay
    /// (the same code again, by the same person or another) finds it used.
    pub(super) fn redeem(
        &mut self,
        person_id: &str,
        now: FirestoreInstant,
    ) -> Result<(), ApiError> {
        if !self.is_valid(now) {
            return Err(invite_invalid());
        }
        self.status = InviteStatus::Used;
        self.used_by = Some(person_id.to_string());
        self.used_at = Some(now);
        Ok(())
    }

    /// Withdraws an invite that nobody has used yet.
    pub(super) fn cancel(&mut self, now: FirestoreInstant) -> Result<(), ApiError> {
        if !self.is_valid(now) {
            return Err(invite_invalid());
        }
        self.status = InviteStatus::Canceled;
        Ok(())
    }

    /// Whether someone with this `previous` membership of the shelter may come in by this invite:
    /// not if already in, and not again by link after being removed (spec 3.3.7).
    pub(super) fn admits(&self, previous: Option<MembershipStatus>) -> Result<(), ApiError> {
        match (previous, self.kind) {
            (Some(MembershipStatus::Active), _) => {
                Err(ApiError::new(StatusCode::CONFLICT, "already_member"))
            }
            (Some(MembershipStatus::Removed), InviteKind::Link) => {
                Err(ApiError::new(StatusCode::FORBIDDEN, "removed_from_shelter"))
            }
            _ => Ok(()),
        }
    }

    /// The membership the invite grants.
    pub(super) fn membership(
        &self,
        code: &str,
        person_id: &str,
        now: FirestoreInstant,
    ) -> MembershipDoc {
        MembershipDoc {
            shelter_id: self.shelter_id.clone(),
            volunteer_id: person_id.to_string(),
            role: self.role,
            status: MembershipStatus::Active,
            via: self.via(),
            invite_code: Some(code.to_string()),
            invited_by: Some(self.created_by.clone()),
            joined_at: now,
            removed_by: None,
            removed_at: None,
        }
    }
}

/// 128 random bits, base64url (spec 7).
pub(super) fn new_code() -> String {
    let mut bytes = [0u8; 16];
    rand::fill(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}
