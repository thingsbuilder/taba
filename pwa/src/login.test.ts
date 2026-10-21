import { beforeEach, describe, expect, it, vi } from 'vitest'
import { type SessionResponse, createApi } from './api'
import type { Db } from './db'
import { getLanguage, loadLanguage, saveLanguage, setLanguage } from './i18n'
import { finishLogin, loginBlock, startLogin } from './login'
import { getActive } from './sessions'
import { freshDb } from './testing'

const NOW = 1_800_000_000_000
const session = (language: 'pt-BR' | 'en' = 'pt-BR'): SessionResponse => ({
  session: 'secret-ana',
  expires_at: new Date(NOW + 7 * 24 * 3600_000).toISOString(),
  volunteer: { id: 'ana', name: 'Ana', phone: '11', show_phone: true, language },
})

const json = (status: number, body: unknown) => new Response(JSON.stringify(body), { status })

let db: Db
beforeEach(async () => {
  db = await freshDb()
})

function deps(replies: Response[]) {
  const fetchFn = vi.fn<typeof fetch>()
  replies.forEach((r) => fetchFn.mockResolvedValueOnce(r))
  const sleeps: number[] = []
  return {
    fetchFn,
    sleeps,
    deps: {
      db,
      api: createApi(fetchFn, async () => 'device-1'),
      sleep: async (ms: number) => void sleeps.push(ms),
      now: () => NOW,
    },
  }
}

describe('loginBlock (offline or degraded disables "Sign in")', () => {
  it.each([
    [true, null, null],
    [true, { unico: 'ok' as const }, null],
    [false, { unico: 'ok' as const }, 'offline'],
    [false, null, 'offline'],
    [true, { unico: 'degraded' as const }, 'degraded'],
    [false, { unico: 'degraded' as const }, 'offline'],
  ])('online=%s status=%j -> %s', (online, status, expected) => {
    expect(loginBlock(online, status)).toBe(expected)
  })
})

describe('startLogin', () => {
  it('creates the capture, remembers it and returns the journey link', async () => {
    const { deps: d, fetchFn } = deps([json(201, { capture_id: 'c1', link: 'https://journey.test/x', expires_at: 'soon' })])
    expect(await startLogin(d)).toEqual({ ok: true, link: 'https://journey.test/x' })
    expect(fetchFn).toHaveBeenCalledWith('/v1/captures', expect.objectContaining({ method: 'POST' }))
    expect(await db.get('captures', 'c1')).toEqual({ capture_id: 'c1', created_at: NOW })
  })

  it('passes the server error up and remembers nothing', async () => {
    const { deps: d } = deps([json(503, { error: 'unico_busy', detail: {} })])
    expect(await startLogin(d)).toEqual({ ok: false, error: 'unico_busy' })
    expect(await db.getAll('captures')).toEqual([])
  })
})

describe('finishLogin', () => {
  it('trades the capture for a session, makes the volunteer active and forgets the capture', async () => {
    await db.put('captures', { capture_id: 'c1', created_at: NOW })
    const { deps: d, fetchFn } = deps([json(201, session())])
    const outcome = await finishLogin(d, 'c1', { name: 'Ana', show_phone: true, language: 'pt-BR' })
    expect(outcome).toMatchObject({ kind: 'logged_in', session: { volunteer_id: 'ana', secret: 'secret-ana' } })
    expect(JSON.parse(fetchFn.mock.calls[0]![1]!.body as string).capture_id).toBe('c1')
    expect((await getActive(db, NOW))?.volunteer_id).toBe('ana')
    expect(await db.get('captures', 'c1')).toBeUndefined()
  })

  it('waits 2 s, 4 s, 8 s, 8 s... while the capture is pending', async () => {
    const pending = () => json(202, { error: 'capture_pending', detail: {} })
    const { deps: d, sleeps } = deps([pending(), pending(), pending(), pending(), json(201, session())])
    expect((await finishLogin(d, 'c1')).kind).toBe('logged_in')
    expect(sleeps).toEqual([2000, 4000, 8000, 8000])
  })

  it('gives up after two minutes of waiting', async () => {
    const fetchFn = vi.fn<typeof fetch>().mockImplementation(async () => json(202, { error: 'capture_pending', detail: {} }))
    const sleeps: number[] = []
    const outcome = await finishLogin(
      { db, api: createApi(fetchFn, async () => 'd'), sleep: async (ms) => void sleeps.push(ms) },
      'c1',
    )
    expect(outcome).toEqual({ kind: 'timeout' })
    expect(sleeps.reduce((a, b) => a + b, 0)).toBeGreaterThanOrEqual(120_000)
    expect(sleeps.reduce((a, b) => a + b, 0)).toBeLessThan(130_000)
  })

  it('asks for the profile when the volunteer is new and keeps the capture for the second call', async () => {
    await db.put('captures', { capture_id: 'c1', created_at: NOW })
    const { deps: d } = deps([json(422, { error: 'profile_required', detail: {} })])
    expect(await finishLogin(d, 'c1')).toEqual({ kind: 'profile_required' })
    expect(await db.get('captures', 'c1')).toBeDefined()
    expect(await getActive(db, NOW)).toBeNull()
  })

  it.each(['capture_inconclusive', 'capture_failed', 'capture_canceled', 'banned', 'capture_consumed', 'unico_busy'])(
    'reports %s as an error without a session',
    async (code) => {
      const { deps: d } = deps([json(422, { error: code, detail: {} })])
      expect(await finishLogin(d, 'c1')).toEqual({ kind: 'error', code })
      expect(await getActive(db, NOW)).toBeNull()
    },
  )

  it('reports a network failure as an error', async () => {
    const fetchFn = vi.fn<typeof fetch>().mockRejectedValue(new TypeError('offline'))
    const outcome = await finishLogin({ db, api: createApi(fetchFn, async () => 'd'), sleep: async () => {} }, 'c1')
    expect(outcome).toEqual({ kind: 'error', code: 'network' })
  })

  it('the language of the profile wins over the one this device had (spec 11.1)', async () => {
    await saveLanguage(db, 'en')
    setLanguage('en')
    const { deps: d } = deps([json(201, session('pt-BR'))])
    await finishLogin(d, 'c1')
    expect(getLanguage()).toBe('pt-BR')
    expect(await loadLanguage(db, ['en-US'])).toBe('pt-BR')
  })

  it('also switches to English when that is the profile language', async () => {
    await saveLanguage(db, 'pt-BR')
    setLanguage('pt-BR')
    const { deps: d } = deps([json(201, session('en'))])
    await finishLogin(d, 'c1')
    expect(getLanguage()).toBe('en')
    expect(await loadLanguage(db, ['pt-BR'])).toBe('en')
  })

  it('never stores a raw token: the session record holds the secret and the profile only', async () => {
    const { deps: d } = deps([json(201, session())])
    await finishLogin(d, 'c1')
    const saved = JSON.stringify(await db.get('config', 'sessions'))
    expect(Object.keys(JSON.parse(saved).ana).sort()).toEqual(
      ['expires_at', 'failed_pins', 'global_roles', 'profile', 'secret', 'ttl_ms', 'volunteer_id'].sort(),
    )
  })
})
