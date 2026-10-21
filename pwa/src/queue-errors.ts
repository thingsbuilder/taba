import { errorKey } from './app-context'
import type { QueueItem } from './db'

// How the "Pendências" list names the phase 3 items and their errors (spec 9.2, 9.4).

const CODES = new Set([
  'person_in_other_household',
  'household_has_card',
  'code_taken',
  'photo_unreadable',
  'no_face',
  'invalid_image',
  'image_too_large',
  'revalidation_failed',
  'protection_role_required',
  'guardian_not_verified',
  'event_closed',
])

/** i18n key of the message for a queue error; the codes of earlier phases keep their `error.*` key. */
export const queueErrorKey = (code: string): string => (CODES.has(code) ? `queue.error_${code}` : errorKey(code))

const RULES: [RegExp, string, string?][] = [
  [/^\/v1\/shelters\/[^/]+\/checkins$/, 'queue.label_checkin'],
  [/^\/v1\/people\/[^/]+\/biometrics$/, 'queue.label_biometrics'],
  [/^\/v1\/people\/[^/]+$/, 'queue.label_person_edit', 'PATCH'],
  [/^\/v1\/people\/[^/]+\/guardians$/, 'queue.label_guardian'],
  [/^\/v1\/shelters\/[^/]+\/exits$/, 'queue.label_exit'],
  [/^\/v1\/shelters\/[^/]+\/households$/, 'queue.label_household'],
  [/^\/v1\/households\/[^/]+$/, 'queue.label_household_edit', 'PATCH'],
  [/^\/v1\/households\/[^/]+\/cards$/, 'queue.label_card'],
  [/^\/v1\/cards\/[^/]+\/cancel$/, 'queue.label_card_cancel'],
]

/** i18n key of what a phase 3 item does; null for the items of earlier phases. */
export function phase3Label(item: Pick<QueueItem, 'method' | 'path'>): string | null {
  return RULES.find(([re, , method]) => re.test(item.path) && (method === undefined || method === item.method))?.[1] ?? null
}
