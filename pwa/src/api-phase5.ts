// Request and response types of the phase 5 routes (spec 7, 8.6, doc da fase 5). Types only, no
// logic. This file is the contract between the API owners and the screens: the API answers these
// shapes, and a change to one changes both sides.

export type ItemScope = 'per_person' | 'per_household'
export type Verification = 'card' | 'liveness' | 'admin_approval'
export type DeliveryStatus = 'ok' | 'in_review' | 'reviewed_ok' | 'irregular'
export type ReasonKind = 'duplicate_in_period' | 'liveness_unavailable'
export type ReviewDecision = 'reviewed_ok' | 'irregular'

/** One item of `shelters.items` (3.5.2). `period_days` is 7 when absent; it is ignored for `once_per_event`. */
export interface ItemSpec {
  scope: ItemScope
  period_days?: number
  once_per_event: boolean
  high_value: boolean
}

export type Items = Record<string, ItemSpec>

/** `PUT /v1/shelters/{id}/items` replaces the whole map; answers the saved one. */
export interface PutItemsRequest {
  items: Items
}

export interface PutItemsResponse {
  items: Items
}

/** `POST /v1/shelters/{id}/deliveries`. The `Idempotency-Key` is the queue item id; `id` is a UUID of the client. */
export interface DeliveryRequest {
  id: string
  card_code: string
  person_id: string
  item: string
  quantity: number
  delivered_at: string
  /** After `428 liveness_required`: the capture of `purpose = revalidation` for `person_id`. */
  capture_id?: string
  /** An admin of the shelter, with their own session, vouches for the person. */
  admin_approval?: true
  /** The delivery was made without signal or with Unico out (spec 3.5.3 item 7): no draw, no selfie asked. */
  offline?: true
}

/** `detail` of `428 liveness_required`: why, and whether a selfie can settle it (not for a person without biometrics). */
export interface LivenessRequiredDetail {
  reason: 'high_value' | 'sample'
  admin_approval_only: boolean
}

export interface DeliveryReason {
  kind: ReasonKind
  previous_delivery_id?: string
  /** For "sent to review: another delivery on <date>". Left out for an unconfirmed requester unless the other delivery is its own (3.5.3 item 6). */
  previous_delivered_at?: string
  /** Only when the requester is confirmed and so is the other shelter, or the other delivery is the requester's own (4.7). */
  previous_shelter_name?: string
}

export interface DeliveryResponse {
  delivery_id: string
  status: DeliveryStatus
  reasons: DeliveryReason[]
}

/** An item of `GET /v1/shelters/{id}/deliveries?status=`, for the admin's review list. */
export interface DeliveryView {
  id: string
  item: string
  scope: ItemScope
  quantity: number
  status: DeliveryStatus
  verification: Verification
  reasons: DeliveryReason[]
  delivered_at: string
  /** Names for the admin to check the delivery, never a person id. */
  person_name?: string
  card_code?: string
  delivered_by_name?: string
  approved_by_name?: string
  review_note?: string
}

export interface ReviewRequest {
  decision: ReviewDecision
  note: string
}
