import type { Api, Membership, ProfileInput, SessionResponse } from './api'
import type { Db } from './db'
import { saveLanguage, setLanguage } from './i18n'
import { type LocalSession, addLoginSession } from './sessions'

// Login (spec 3.1): create a capture, open the Unico journey, come back on /return?capture=<id>,
// then trade the capture for a session. The journey returns in a fresh page load, so the only
// state it needs is the `capture` in the URL and what is in IndexedDB.

const POLL_FIRST_MS = 2_000
const POLL_MAX_STEP_MS = 8_000
const POLL_LIMIT_MS = 120_000

export interface LoginDeps {
  db: Db
  api: Api
  sleep?: (ms: number) => Promise<void>
  now?: () => number
}

/** Why "Sign in" is disabled, if it is (spec 3.1, step 6). */
export function loginBlock(online: boolean, status: { unico: 'ok' | 'degraded' } | null): 'offline' | 'degraded' | null {
  if (!online) return 'offline'
  return status?.unico === 'degraded' ? 'degraded' : null
}

export type StartOutcome = { ok: true; link: string } | { ok: false; error: string }

/** With `inviteCode` the capture is for accepting that invite (spec 3.3); `/return` finds it again by capture id. */
export async function startLogin({ db, api, now = Date.now }: LoginDeps, inviteCode?: string): Promise<StartOutcome> {
  const result = await api.createCapture(inviteCode ? 'invite' : 'login')
  if (!result.ok) return { ok: false, error: result.error }
  await db.put('captures', {
    capture_id: result.data.capture_id,
    created_at: now(),
    ...(inviteCode ? { purpose: 'invite' as const, invite_code: inviteCode } : {}),
  })
  return { ok: true, link: result.data.link }
}

export type LoginOutcome =
  | { kind: 'logged_in'; session: LocalSession; membership?: Membership }
  | { kind: 'profile_required' }
  | { kind: 'timeout' }
  | { kind: 'error'; code: string }

/** Polls while Unico has not finished (`202 capture_pending`: 2 s, 4 s, 8 s, ... up to 2 min). */
export async function finishLogin(deps: LoginDeps, captureId: string, profile?: ProfileInput, inviteCode?: string): Promise<LoginOutcome> {
  const { db, api, sleep = (ms) => new Promise((r) => setTimeout(r, ms)), now = Date.now } = deps
  let waited = 0
  for (let step = POLL_FIRST_MS; ; step = Math.min(step * 2, POLL_MAX_STEP_MS)) {
    const result = inviteCode ? await api.acceptInvite(inviteCode, captureId, profile) : await api.createSession(captureId, profile)
    if (result.ok) {
      const data: SessionResponse & { membership?: Membership } = result.data
      const session = await addLoginSession(db, data, now())
      await db.delete('captures', captureId)
      // The profile's language wins over what this device had (spec 11.1).
      setLanguage(session.profile.language)
      await saveLanguage(db, session.profile.language)
      return { kind: 'logged_in', session, membership: data.membership }
    }
    if (result.error === 'profile_required') return { kind: 'profile_required' }
    if (result.error !== 'capture_pending') return { kind: 'error', code: result.error }
    if (waited >= POLL_LIMIT_MS) return { kind: 'timeout' }
    await sleep(step)
    waited += step
  }
}
