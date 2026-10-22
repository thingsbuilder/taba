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
  'guardian_not_verified',
  'protection_role_required',
  'photo_too_big',
  'invalid_request',
  'person_not_found',
  'household_not_found',
  'not_in_shelter',
  'person_mismatch',
  'capture_target_required',
  'capture_wrong_target',
  'invalid_person',
  'invalid_household',
  'invalid_card',
  'invalid_guardian',
  'household_exists',
  'card_not_found',
  'thumbnail_not_found',
  'invalid_thumbnail',
  'person_changed',
  'revalidation_failed',
  'person_in_other_household',
  'household_has_card',
  'code_taken',
  'exit_destination_required',
  'guardian_required',
  'guardian_not_linked',
  'capture_required',
  'exception_for_child_only',
  'exception_reason_required',
  'wrong_capture_target',
  'invalid_destination_municipality',
  'event_closed',
  'photo_rejected',
  'search_limit',
  'match_already_decided',
  'search_link_invalid',
  'too_many_requests',
  'rescue_not_found',
  'search_id_taken',
  'liveness_required',
  'admin_approval_required',
  'person_not_in_household',
  'delivery_not_found',
  'delivery_not_in_review',
  'delivery_id_taken',
  'idempotency_key_reused',
  'idempotency_in_progress',
  'merge_too_large',
])

/** i18n key for a stable server error code; unknown codes get the generic message. */
export const errorKey = (code: string): string => (KNOWN_ERRORS.has(code) ? `error.${code}` : 'error.generic')
