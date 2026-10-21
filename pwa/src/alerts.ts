import type { AlertView } from './api'

// Text key and link for an alert in the inbox (spec 8.8). Unknown kinds fall back to a generic text.

const KNOWN = new Set([
  'shelter_flagged', 'event_overlap', 'volunteer_removed', 'endorser_flagged',
  'child_separated', 'revalidation_failed', 'exceptional_exit', 'household_conflict', 'unico_limit',
])

export const alertTextKey = (kind: string): string => (KNOWN.has(kind) ? `alert.${kind}` : 'alert.generic')

/** Alerts about a person open the person; those about a shelter, its screen; the others have no destination. */
export function alertLink(alert: AlertView): string | null {
  if (alert.target_kind === 'person') return `/person/${alert.target_id}`
  if (alert.target_kind === 'shelter') return `/shelter/${alert.target_id}`
  return alert.shelter_ids.length === 1 ? `/shelter/${alert.shelter_ids[0]}` : null
}

export interface AlertPhone {
  textKey: string
  phone: string
}

/** The phones a `child_separated` alert carries (`detail.reference_contacts`, spec 3.4.1, item 3): who to call. */
export function alertPhones(alert: AlertView): AlertPhone[] {
  if (alert.kind !== 'child_separated') return []
  const contacts = (alert.detail as { reference_contacts?: { civil_defense_phone?: string; child_protection_phone?: string } }).reference_contacts
  return [
    ...(contacts?.child_protection_phone ? [{ textKey: 'alert.phone_child_protection', phone: contacts.child_protection_phone }] : []),
    ...(contacts?.civil_defense_phone ? [{ textKey: 'alert.phone_civil_defense', phone: contacts.civil_defense_phone }] : []),
  ]
}

/** `m:ss` left until `expiresAt`, never negative. */
export function countdown(expiresAt: string, now: number): string {
  const seconds = Math.max(0, Math.ceil((Date.parse(expiresAt) - now) / 1000))
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`
}
