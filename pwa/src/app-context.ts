import type { Api, ApiStatus } from './api'
import type { Db } from './db'
import type { createNominatim } from './nominatim'
import type { Queue } from './queue'
import type { LocalSession } from './sessions'

/** What every screen gets from `App`. */
export interface AppContext {
  db: Db
  api: Api
  queue: Queue
  nominatim: ReturnType<typeof createNominatim>
  /** The operator who signs new writes; null when nobody is logged in. */
  active: LocalSession | null
  /** Volunteers with a valid session on this device (for "switch operator"). */
  sessions: LocalSession[]
  status: ApiStatus | null
  refresh: () => Promise<void>
}

const KNOWN_ERRORS = new Set([
  'network',
  'banned',
  'capture_inconclusive',
  'capture_failed',
  'capture_canceled',
  'capture_expired',
  'capture_consumed',
  'capture_wrong_device',
  'capture_not_found',
  'unico_busy',
  'unico_limit',
  'unico_degraded',
  'unico_unavailable',
  'invalid_profile',
  'insufficient_accuracy',
  'shelter_nearby',
  'too_far',
  'cannot_endorse',
  'link_volunteer_only',
  'invite_invalid',
  'already_member',
  'removed_from_shelter',
  'admin_cannot_remove_admin',
  'last_admin',
  'forbidden',
  'shelter_not_found',
  'member_not_found',
  'endorsement_code_invalid',
  'already_confirmed',
  'invalid_shelter',
  'wrong_capture_purpose',
  'shelter_exists',
  'unauthorized',
  'idempotency_key_mismatch',
  'invalid_idempotency_key',
  'alert_not_found',
  'event_not_found',
  'invalid_event',
  'invalid_reason',
  'invalid_role',
  'contention',
])

/** i18n key for a stable server error code; unknown codes get the generic message. */
export const errorKey = (code: string): string => (KNOWN_ERRORS.has(code) ? `error.${code}` : 'error.generic')
