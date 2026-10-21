import type { Api, ApiStatus } from './api'
import type { Db } from './db'
import type { Queue } from './queue'
import type { LocalSession } from './sessions'

/** What every screen gets from `App`. */
export interface AppContext {
  db: Db
  api: Api
  queue: Queue
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
])

/** i18n key for a stable server error code; unknown codes get the generic message. */
export const errorKey = (code: string): string => (KNOWN_ERRORS.has(code) ? `error.${code}` : 'error.generic')
