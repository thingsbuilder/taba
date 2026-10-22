//! The JSON shown of an event.

use serde_json::{Value, json};

use super::model::EventDoc;

impl EventDoc {
    pub fn view(&self, id: &str) -> Value {
        json!({
            "id": id,
            "name": self.name,
            "auto_named": self.auto_named,
            "status": self.status,
            "reference_contacts": self.reference_contacts,
        })
    }
}
