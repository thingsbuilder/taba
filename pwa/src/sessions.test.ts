import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { SessionResponse } from './api'
import type { Db } from './db'
import { switchFailure } from './switch-message'
import { MAX_FAILED_PINS, needsPin, addLoginSession, applyMe, getActive, getSession, hasPin, listValid, removeSession, setActive, setPin, switchOperator, touchSession, updateProfile } from './sessions'
import { freshDb } from './testing'

const DAY = 24 * 3600_000
const NOW = 1_800_000_000_000

const response = (id: string, name = id, ttl = 7 * DAY): SessionResponse => ({
  session: `secret-${id}`,
  expires_at: new Date(NOW + ttl).toISOString(),
  volunteer: { id, name, phone: null, show_phone: false, language: 'pt-BR' },
})

let db: Db
beforeEach(async () => {
  db = await freshDb()
})

describe('login sessions', () => {
  it('stores the session, makes the volunteer active and remembers the lifetime', async () => {
    const session = await addLoginSession(db, response('ana'), NOW)
    expect(session).toMatchObject({ volunteer_id: 'ana', secret: 'secret-ana', expires_at: NOW + 7 * DAY, ttl_ms: 7 * DAY, failed_pins: 0 })
    expect(session.profile).toEqual({ name: 'ana', phone: null, show_phone: false, language: 'pt-BR' })
    expect(hasPin(session)).toBe(false)
    expect((await getActive(db, NOW))?.volunteer_id).toBe('ana')
  })

  it('keeps one session per volunteer on the same device', async () => {
    await addLoginSession(db, response('ana'), NOW)
    await addLoginSession(db, response('bia'), NOW)
    expect((await listValid(db, NOW)).map((s) => s.volunteer_id).sort()).toEqual(['ana', 'bia'])
    expect((await getActive(db, NOW))?.volunteer_id).toBe('bia')
  })

  it('a second login by the same person keeps the PIN made on this device', async () => {
    await addLoginSession(db, response('ana'), NOW)
    await setPin(db, 'ana', '123456')
    const again = await addLoginSession(db, { ...response('ana'), session: 'new-secret' }, NOW + DAY)
    expect(again.secret).toBe('new-secret')
    expect(hasPin(again)).toBe(true)
    expect(await switchOperator(db, 'ana', '123456', NOW + DAY)).toEqual({ ok: true })
  })

  it('does not list or activate an expired session', async () => {
    await addLoginSession(db, response('ana'), NOW)
    const later = NOW + 7 * DAY + 1
    expect(await listValid(db, later)).toEqual([])
    expect(await getActive(db, later)).toBeNull()
    expect(await getSession(db, 'ana', later)).toBeNull()
  })

  it('removing the active session leaves nobody active', async () => {
    await addLoginSession(db, response('ana'), NOW)
    await removeSession(db, 'ana')
    expect(await getActive(db, NOW)).toBeNull()
    expect(await db.get('config', 'active_volunteer_id')).toBeUndefined()
  })

  it('removing another volunteer keeps the active one', async () => {
    await addLoginSession(db, response('ana'), NOW)
    await addLoginSession(db, response('bia'), NOW)
    await removeSession(db, 'ana')
    expect((await getActive(db, NOW))?.volunteer_id).toBe('bia')
    expect((await listValid(db, NOW)).map((s) => s.volunteer_id)).toEqual(['bia'])
  })

  it('follows the server renewal: at most once a day, never backwards', async () => {
    await addLoginSession(db, response('ana'), NOW)
    await touchSession(db, 'ana', NOW + 3 * DAY)
    expect((await getSession(db, 'ana', NOW))?.expires_at).toBe(NOW + 3 * DAY + 6 * DAY)
    await touchSession(db, 'ana', NOW + DAY)
    expect((await getSession(db, 'ana', NOW))?.expires_at).toBe(NOW + 9 * DAY)
    await touchSession(db, 'nobody', NOW)
  })

  it('stores the roles from /me and the edited profile', async () => {
    await addLoginSession(db, response('ana'), NOW)
    await applyMe(db, 'ana', ['civil_defense'], NOW)
    await updateProfile(db, 'ana', { name: 'Ana Souza', language: 'en' })
    const session = await getSession(db, 'ana', NOW)
    expect(session?.global_roles).toEqual(['civil_defense'])
    expect(session?.profile).toMatchObject({ name: 'Ana Souza', language: 'en', show_phone: false })
  })
})

describe('switching operator by PIN (spec 15, criterion 57)', () => {
  beforeEach(async () => {
    await addLoginSession(db, response('ana'), NOW)
    await setPin(db, 'ana', '123456')
    await addLoginSession(db, response('bia'), NOW)
    await setPin(db, 'bia', '654321')
  })

  it('switches with the right PIN and needs no network', async () => {
    const network = vi.fn(() => Promise.reject(new Error('offline')))
    vi.stubGlobal('fetch', network)
    expect(await switchOperator(db, 'ana', '123456', NOW)).toEqual({ ok: true })
    expect((await getActive(db, NOW))?.volunteer_id).toBe('ana')
    expect(network).not.toHaveBeenCalled()
    vi.unstubAllGlobals()
  })

  it('a wrong PIN does not switch and says how many tries are left', async () => {
    expect(await switchOperator(db, 'ana', '000000', NOW)).toEqual({ ok: false, reason: 'wrong_pin', remaining: 4 })
    expect((await getActive(db, NOW))?.volunteer_id).toBe('bia')
    expect(await switchOperator(db, 'ana', '000000', NOW)).toEqual({ ok: false, reason: 'wrong_pin', remaining: 3 })
  })

  it('five wrong PINs in a row erase that volunteer session on the device', async () => {
    for (let i = 1; i < MAX_FAILED_PINS; i++) {
      expect(await switchOperator(db, 'ana', '000000', NOW)).toMatchObject({ reason: 'wrong_pin' })
    }
    expect(await switchOperator(db, 'ana', '000000', NOW)).toEqual({ ok: false, reason: 'locked' })
    expect(await getSession(db, 'ana', NOW)).toBeNull()
    expect((await listValid(db, NOW)).map((s) => s.volunteer_id)).toEqual(['bia'])
    // Even the right PIN no longer works: there is no session left.
    expect(await switchOperator(db, 'ana', '123456', NOW)).toEqual({ ok: false, reason: 'no_session' })
  })

  it('erasing the active volunteer leaves nobody active, the others keep their sessions', async () => {
    await switchOperator(db, 'ana', '123456', NOW)
    for (let i = 0; i < MAX_FAILED_PINS; i++) await switchOperator(db, 'ana', '000000', NOW)
    expect(await getActive(db, NOW)).toBeNull()
    expect((await getSession(db, 'bia', NOW))?.secret).toBe('secret-bia')
  })

  it('wrong PINs for one volunteer do not count against another', async () => {
    for (let i = 0; i < 3; i++) await switchOperator(db, 'ana', '000000', NOW)
    expect(await switchOperator(db, 'bia', '000000', NOW)).toEqual({ ok: false, reason: 'wrong_pin', remaining: 4 })
  })

  it('a right PIN resets the count', async () => {
    for (let i = 0; i < 4; i++) await switchOperator(db, 'ana', '000000', NOW)
    expect(await switchOperator(db, 'ana', '123456', NOW)).toEqual({ ok: true })
    expect(await switchOperator(db, 'ana', '000000', NOW)).toEqual({ ok: false, reason: 'wrong_pin', remaining: 4 })
  })

  it('the failed attempt is stored before the PIN is checked', async () => {
    // Closing the app in the middle of a guess must not give a free try.
    const attempt = switchOperator(db, 'ana', '000000', NOW)
    await attempt
    expect((await getSession(db, 'ana', NOW))?.failed_pins).toBe(1)
  })

  it('refuses an unknown volunteer, an expired session and a session with no PIN', async () => {
    expect(await switchOperator(db, 'zed', '123456', NOW)).toEqual({ ok: false, reason: 'no_session' })
    expect(await switchOperator(db, 'ana', '123456', NOW + 8 * DAY)).toEqual({ ok: false, reason: 'no_session' })
    await addLoginSession(db, response('cai'), NOW)
    expect(await switchOperator(db, 'cai', '123456', NOW)).toEqual({ ok: false, reason: 'no_pin' })
    await setActive(db, 'bia')
  })
})

describe('a session whose owner never created the PIN', () => {
  it('is flagged so the app asks for the PIN before anything else', async () => {
    const session = await addLoginSession(db, response('ana'), NOW)
    expect(needsPin(session)).toBe(true)
    expect(needsPin(null)).toBe(false)
    await setPin(db, 'ana', '123456')
    expect(needsPin(await getActive(db, NOW))).toBe(false)
  })

  it('is not "session ended": the switch screen gets its own message', async () => {
    await addLoginSession(db, response('ana'), NOW)
    const result = await switchOperator(db, 'ana', '123456', NOW)
    expect(result).toEqual({ ok: false, reason: 'no_pin' })
    if (result.ok) throw new Error('unreachable')
    expect(switchFailure(result).key).toBe('switch.no_pin')
    expect(switchFailure({ ok: false, reason: 'no_session' }).key).toBe('switch.no_session')
    expect(switchFailure({ ok: false, reason: 'locked' }).key).toBe('switch.locked')
    expect(switchFailure({ ok: false, reason: 'wrong_pin', remaining: 2 })).toEqual({ key: 'switch.wrong', params: { n: 2 } })
  })
})
