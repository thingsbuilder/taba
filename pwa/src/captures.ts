import type { Api, ApiResult } from './api'
import type { Db } from './db'

// Captures with liveness for check-in, revalidation and late biometrics (spec 3.4, 3.8, 4.12). The Unico journey
// leaves the app and comes back in a fresh page load on `/return?capture=<id>`, so the screen's state waits in
// IndexedDB: the capture record says where to return, the draft says what was being done.

export type CaptureKind = 'checkin' | 'revalidation' | 'biometrics'

const POLL_FIRST_MS = 2_000
const POLL_MAX_STEP_MS = 8_000
const POLL_LIMIT_MS = 120_000

export const draftKey = (captureId: string) => `draft:${captureId}`

export type StartResult = { ok: true; link: string } | { ok: false; error: string }

/** Creates the capture, saves where to come back to and the draft, and gives the Unico link to open. */
export async function startCapture(
  deps: { db: Db; api: Api; now?: () => number },
  secret: string,
  kind: CaptureKind,
  opts: { shelter_id?: string; expected_person_id?: string },
  returnTo: string,
  draft: unknown,
): Promise<StartResult> {
  const res = await deps.api.createCapture(kind, { secret, ...opts })
  if (!res.ok) return { ok: false, error: res.error }
  const id = res.data.capture_id
  await deps.db.put('captures', { capture_id: id, created_at: (deps.now ?? Date.now)(), purpose: kind, return_to: returnTo })
  await deps.db.put('config', draft, draftKey(id))
  return { ok: true, link: res.data.link }
}

export async function readDraft<T>(db: Db, captureId: string): Promise<T | null> {
  return ((await db.get('config', draftKey(captureId))) as T | undefined) ?? null
}

/** The capture was used (or is no longer wanted): forget the record and the draft. */
export async function finishCapture(db: Db, captureId: string): Promise<void> {
  await db.delete('captures', captureId)
  await db.delete('config', draftKey(captureId))
}

/** Where `/return` sends a capture made for one of these purposes; null for a login or an invite. */
export async function returnPathOf(db: Db, captureId: string): Promise<string | null> {
  const record = await db.get('captures', captureId)
  const path = record?.return_to
  // Only in-app paths: anything else would be an open redirect.
  return record?.purpose && record.purpose !== 'invite' && typeof path === 'string' && path.startsWith('/') && !path.startsWith('//')
    ? `${path}${path.includes('?') ? '&' : '?'}capture=${encodeURIComponent(captureId)}`
    : null
}

export type PollResult<T> = { ok: true; data: T } | { ok: false; error: string; status: number } | { ok: false; error: 'timeout'; status: 0 }

/** Repeats the call while Unico has not finished (`202 capture_pending`: 2 s, 4 s, 8 s, ... up to 2 min). */
export async function pollCapture<T>(
  call: () => Promise<ApiResult<T>>,
  sleep: (ms: number) => Promise<void> = (ms) => new Promise((r) => setTimeout(r, ms)),
): Promise<PollResult<T>> {
  let waited = 0
  for (let step = POLL_FIRST_MS; ; step = Math.min(step * 2, POLL_MAX_STEP_MS)) {
    const res = await call()
    if (res.ok) return { ok: true, data: res.data }
    if (res.error !== 'capture_pending') return { ok: false, error: res.error, status: res.status }
    if (waited >= POLL_LIMIT_MS) return { ok: false, error: 'timeout', status: 0 }
    await sleep(step)
    waited += step
  }
}
