//! The transaction behind a grant or a revoke: the volunteer and its audit entry change
//! together.

use axum::http::StatusCode;

use crate::audit::{self, AuditEntry, LegalBasis};
use crate::auth::{AppState, Volunteer};
use crate::error::ApiError;
use crate::store::{self, tx_replace};
use crate::volunteers::{COLLECTION as VOLUNTEERS, GlobalRole, VolunteerDoc};

/// Grants or revokes `role` on the target in one transaction with its audit entry. Asking for
/// the state the target already is in changes and records nothing.
pub(super) async fn change(
    state: &AppState,
    by: &Volunteer,
    target: &str,
    role: GlobalRole,
    grant: bool,
) -> Result<(), ApiError> {
    store::transact(&state.db, |db, tx| {
        let (target, by) = (target.to_string(), by.id.clone());
        Box::pin(async move {
            let mut doc: VolunteerDoc = store::get(&db, VOLUNTEERS, &target)
                .await?
                .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "volunteer_not_found"))?;
            if !doc.set_role(role, grant, &by, store::now()) {
                return Ok(());
            }
            tx_replace(&db, tx, VOLUNTEERS, &target, &doc)?;
            let verb = if grant { "grant" } else { "revoke" };
            audit::add(
                &db,
                tx,
                &AuditEntry::new(format!("role.{verb}.{}", role.name()), "volunteer", &target)
                    .by(&by)
                    .basis(LegalBasis::PublicPolicy),
            )
        })
    })
    .await
}
