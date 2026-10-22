//! The rules on a card (spec 3.5.1): the code, as typed and as stored, and what a cancel
//! changes. Plain functions; nothing here is `async` or reads the database.

use firestore::FirestoreInstant;

use super::model::{CardDoc, CardStatus, invalid};
use crate::error::ApiError;

impl CardDoc {
    /// Cancels the card; `false` means it already was, so the first cancel (its time and
    /// `replaced_by`) stays and nothing is written.
    pub(super) fn cancel(&mut self, replaced_by: Option<String>, at: FirestoreInstant) -> bool {
        if self.status == CardStatus::Canceled {
            return false;
        }
        self.status = CardStatus::Canceled;
        self.canceled_at = Some(at);
        self.replaced_by = replaced_by;
        true
    }
}

const CODE_LEN: usize = 8;
const ALPHABET: &str = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

pub(super) fn is_canonical(code: &str) -> bool {
    code.chars().count() == CODE_LEN && code.chars().all(|c| ALPHABET.contains(c))
}

/// What someone typed or scanned, as Crockford reads it: case does not matter and `I`, `L` and
/// `O` stand for `1`, `1` and `0`. Whether the result is a code is `is_canonical`'s to say.
pub fn crockford(input: &str) -> String {
    input
        .trim()
        .chars()
        .map(|c| match c.to_ascii_uppercase() {
            'I' | 'L' => '1',
            'O' => '0',
            other => other,
        })
        .collect()
}

/// `crockford`, refusing anything that is still not a code.
pub(super) fn normalize(input: &str, field: &str) -> Result<String, ApiError> {
    let code = crockford(input);
    is_canonical(&code)
        .then_some(code)
        .ok_or_else(|| invalid(field))
}
