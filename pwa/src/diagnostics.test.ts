import { beforeEach, describe, expect, it } from 'vitest'
import { LOG_LIMIT, clearLog, formatLog, logEvent, readLog, routeOf, sanitize, setDiagnosticsDb, uaSummary } from './diagnostics'
import { createApi } from './api'
import { createQueue } from './queue'
import { freshDb } from './testing'

beforeEach(async () => setDiagnosticsDb(await freshDb()))

describe('the buffer', () => {
  it('keeps the events in order', async () => {
    logEvent('a', { n: 1 })
    logEvent('b')
    expect((await readLog()).map((e) => e.kind)).toEqual(['a', 'b'])
  })

  it('keeps only the last 200', async () => {
    for (let i = 0; i < LOG_LIMIT + 5; i++) logEvent('e', { i })
    const log = await readLog()
    expect(log).toHaveLength(LOG_LIMIT)
    expect(log[0]!.detail.i).toBe(5)
    expect(log.at(-1)!.detail.i).toBe(LOG_LIMIT + 4)
  })

  it('clears', async () => {
    logEvent('a')
    await clearLog()
    expect(await readLog()).toEqual([])
  })

  it('never throws when IndexedDB is not available', async () => {
    const broken = { get: () => Promise.reject(new Error('gone')), put: () => Promise.reject(new Error('gone')), delete: () => Promise.reject(new Error('gone')) }
    setDiagnosticsDb(broken as never)
    expect(() => logEvent('a')).not.toThrow()
    expect(await readLog()).toEqual([])
    await expect(clearLog()).resolves.toBeUndefined()
    setDiagnosticsDb(null)
    expect(() => logEvent('a')).not.toThrow()
    expect(await readLog()).toEqual([])
  })
})

describe('sanitize', () => {
  it('drops names, phones, photos, tokens and bodies, and cuts ids to 6 characters', () => {
    const out = sanitize({
      name: 'Maria da Silva',
      phone: '+55 21 99999-8888',
      photo_base64: 'A'.repeat(100),
      Authorization: 'Bearer abc123',
      body: 'x',
      volunteer_id: '123456789-abcdef',
      person_id: 'abcdef-0000',
      status: 200,
      ok: true,
    })
    expect(out).toEqual({ volunteer_id: '123456', person_id: 'abcdef', status: 200, ok: true })
  })

  it('cleans a secret hidden in a harmless key', () => {
    const out = sanitize({
      a: 'failed with Bearer s3cr3t-token',
      b: 'x'.repeat(60),
      c: 'call +55 21 99999-8888 now',
      d: 'data:image/jpeg;base64,/9j/4AAQ',
      e: 'q '.repeat(500),
    })
    expect(JSON.stringify(out)).not.toMatch(/s3cr3t|99999|9j\/4AAQ|x{40}/)
    expect((out.e as string).length).toBeLessThanOrEqual(120)
  })

  it('logEvent stores nothing sensitive', async () => {
    logEvent('x', { name: 'Maria', phone: '21999998888', note: 'Bearer abc', person_id: 'abcdef0123456789' })
    expect((await readLog())[0]!.detail).toEqual({ person_id: 'abcdef' })
  })
})

describe('routeOf', () => {
  it('cuts ids and hides codes', () => {
    expect(routeOf('/v1/people/0b7f3c1e-1111-2222-3333-444455556666/biometrics')).toBe('/v1/people/0b7f3c/biometrics')
    expect(routeOf('/v1/shelters/abc/checkins')).toBe('/v1/shelters/abc/checkins')
    expect(routeOf('/v1/invites/SECRET-CODE-123')).toBe('/v1/invites/:code')
    expect(routeOf('/v1/cards/ABCD1234?x=1')).toBe('/v1/cards/:code')
  })
})

describe('formatLog', () => {
  it('writes the app, the phone and the events, newest first', () => {
    const ua = 'Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Mobile/15E148 Safari/604.1'
    const text = formatLog(
      [{ t: 0, kind: 'first', detail: {} }, { t: 1000, kind: 'second', detail: { status: 502, route: '/v1/x' } }],
      { version: '0.4.3', userAgent: ua, online: false, now: 2000 },
    )
    expect(text.split('\n')).toEqual([
      'Taba 0.4.3',
      'Browser: iPhone OS 17.4 / Version 17.4',
      'Online: no',
      'Time: 1970-01-01T00:00:02.000Z',
      '',
      '1970-01-01T00:00:01.000Z second status=502 route=/v1/x',
      '1970-01-01T00:00:00.000Z first',
    ])
  })

  it('summarizes an unknown user agent', () => {
    expect(uaSummary('curl/8')).toBe('unknown')
  })
})

describe('what the app logs', () => {
  it('records a failed API call and a queue send, with no ids in the route and no body', async () => {
    const db = await freshDb()
    const fetchFn = (async () => new Response(JSON.stringify({ error: 'unico_down' }), { status: 502 })) as typeof fetch
    await createApi(fetchFn, async () => 'dev').getPerson('SECRET', '0b7f3c1e-1111-2222-3333-444455556666')
    const queue = createQueue({ db, fetch: fetchFn, deviceId: async () => 'dev', getStatus: async () => null, random: () => 0 })
    await queue.enqueue({ method: 'PATCH', path: '/v1/people/0b7f3c1e-1111-2222-3333-444455556666', body: { name: 'Maria' }, volunteer_id: null })
    await queue.drain()
    const log = await readLog()
    expect(log.map((e) => e.kind)).toEqual(['api_error', 'queue_send', 'queue_retry'])
    expect(log[0]!.detail).toMatchObject({ route: '/v1/people/0b7f3c', status: 502, error: 'unico_down' })
    expect(log[2]!.detail).toMatchObject({ attempt: 1, reason: 'unico_down' })
    expect(JSON.stringify(log)).not.toMatch(/Maria|SECRET|0b7f3c1e-1111/)
  })
})
