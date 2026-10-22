//! The admin side of the deliveries (spec 3.5.2, 3.5.3 item 8, 8.6): `PUT /v1/shelters/{id}/items`,
//! `GET /v1/shelters/{id}/deliveries?status=` and `POST /v1/deliveries/{id}/review`. The code is in
//! `deliveries/`, with the rest of the deliveries; this file keeps the paths the routes use.

pub use crate::deliveries::{list_deliveries, put_items, review_delivery};
