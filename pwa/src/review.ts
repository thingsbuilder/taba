import type { DeliveryReason } from './api-phase5'
import { getLanguage } from './i18n'

/** Text of one reason. The shelter name is shown only when the API sent it (it leaves it out for an unconfirmed shelter, 4.7). */
export function reasonText(reason: DeliveryReason): { key: string; params: Record<string, string> } {
  if (reason.kind === 'liveness_unavailable') return { key: 'review.reason_offline', params: {} }
  const date = reason.previous_delivered_at ? new Date(reason.previous_delivered_at).toLocaleDateString(getLanguage()) : ''
  if (reason.previous_shelter_name) return { key: 'review.reason_duplicate_at', params: { date, shelter: reason.previous_shelter_name } }
  return { key: 'review.reason_duplicate', params: { date } }
}

/** The line under a delivery of the review list: who delivered, when (in the language of the app), the card and who approved. */
export function reviewMeta(d: { delivered_by_name?: string; delivered_at: string; card_code?: string; approved_by_name?: string }, approvedLabel: string): string {
  return [d.delivered_by_name, new Date(d.delivered_at).toLocaleString(getLanguage()), d.card_code, d.approved_by_name && `${approvedLabel} ${d.approved_by_name}`]
    .filter(Boolean)
    .join(' · ')
}

/** `409` is a delivery someone already decided; `403` and `404` have their own texts. */
export function reviewErrorKey(status: number): string | null {
  if (status === 409) return 'review.error_decided'
  if (status === 403) return 'review.error_forbidden'
  if (status === 404) return 'review.error_not_found'
  return null
}
