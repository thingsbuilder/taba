import type { AlertView } from './api'

// Text key and link for an alert in the inbox (spec 8.8). Unknown kinds fall back to a generic text.

const KNOWN = new Set(['shelter_flagged', 'event_overlap', 'volunteer_removed', 'endorser_flagged'])

export const alertTextKey = (kind: string): string => (KNOWN.has(kind) ? `alert.${kind}` : 'alert.generic')

/** Alerts about a shelter link to its screen; the others have no destination. */
export function alertLink(alert: AlertView): string | null {
  if (alert.target_kind === 'shelter') return `/shelter/${alert.target_id}`
  return alert.shelter_ids.length === 1 ? `/shelter/${alert.shelter_ids[0]}` : null
}

/** `m:ss` left until `expiresAt`, never negative. */
export function countdown(expiresAt: string, now: number): string {
  const seconds = Math.max(0, Math.ceil((Date.parse(expiresAt) - now) / 1000))
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`
}
