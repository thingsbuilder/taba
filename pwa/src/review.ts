import type { DeliveryReason } from './api-phase5'

/** Text of one reason. The shelter name is shown only when the API sent it (it leaves it out for an unconfirmed shelter, 4.7). */
export function reasonText(reason: DeliveryReason): { key: string; params: Record<string, string> } {
  if (reason.kind === 'liveness_unavailable') return { key: 'review.reason_offline', params: {} }
  const date = reason.previous_delivered_at ? new Date(reason.previous_delivered_at).toLocaleDateString() : ''
  if (reason.previous_shelter_name) return { key: 'review.reason_duplicate_at', params: { date, shelter: reason.previous_shelter_name } }
  return { key: 'review.reason_duplicate', params: { date } }
}

/** `409` is a delivery someone already decided; `403` and `404` have their own texts. */
export function reviewErrorKey(status: number): string | null {
  if (status === 409) return 'review.error_decided'
  if (status === 403) return 'review.error_forbidden'
  if (status === 404) return 'review.error_not_found'
  return null
}
