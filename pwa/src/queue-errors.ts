import { checkinErrorKey } from './checkin'
import type { QueueItem } from './db'

// How the "Pendências" list names the phase 3 items and their errors (spec 9.2, 9.4).

const CODES = new Set([
  'person_in_other_household',
  'household_has_card',
  'code_taken',
  'photo_unreadable',
  'revalidation_failed',
  'protection_role_required',
  'guardian_not_verified',
  'event_closed',
])

/** i18n key of the message for a queue error; the face and image errors share the check-in texts, the codes of earlier phases keep their `error.*` key. */
export const queueErrorKey = (code: string): string => (CODES.has(code) ? `queue.error_${code}` : checkinErrorKey(code))

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
  [/^\/v1\/rescues$/, 'rescue.queue_label'],
  [/^\/v1\/searches$/, 'search.queue_label'],
]

/** i18n key of what a phase 3 item does; null for the items of earlier phases. */
export function phase3Label(item: Pick<QueueItem, 'method' | 'path'>): string | null {
  return RULES.find(([re, , method]) => re.test(item.path) && (method === undefined || method === item.method))?.[1] ?? null
}

/** What Unico says about a photo it will refuse again: never repeated, a person decides (spec 5.3). */
export const PHOTO_ERRORS = new Set(['photo_rejected', 'no_face', 'invalid_image', 'image_too_large'])

/** Photos that may be refused of one registration before only "register without a photo" is left. */
export const MAX_PHOTO_ATTEMPTS = 2

const PHOTO_PATHS: [string, RegExp][] = [
  ['POST', /^\/v1\/shelters\/[^/]+\/checkins$/],
  ['PUT', /^\/v1\/people\/[^/]+\/biometrics$/],
]

/** The ways out of a refused photo, for an item that waits because of it; null for any other item. */
export function photoRefusalActions(item: Pick<QueueItem, 'method' | 'path' | 'state' | 'error' | 'photo_refusals'>): { again: boolean } | null {
  if (item.state !== 'needs_action' || !item.error || !PHOTO_ERRORS.has(item.error)) return null
  if (!PHOTO_PATHS.some(([method, path]) => item.method === method && path.test(item.path))) return null
  return { again: (item.photo_refusals ?? 0) < MAX_PHOTO_ATTEMPTS }
}
