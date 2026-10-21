import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { Db } from './db'
import { backoffMs, createQueue, parseRetryAfter } from './queue'
import { freshDb } from './testing'

describe('backoffMs', () => {
  it('doubles from 5 s per attempt (jitter factor 1.0 at random = 0.5)', () => {
    expect(backoffMs(0, () => 0.5)).toBe(5_000)
    expect(backoffMs(1, () => 0.5)).toBe(10_000)
    expect(backoffMs(3, () => 0.5)).toBe(40_000)
  })
  it('caps the base at 10 minutes before the jitter', () => {
    expect(backoffMs(7, () => 0.5)).toBe(600_000)
    expect(backoffMs(50, () => 0.5)).toBe(600_000)
  })
  it('jitter spans 0.5x to 1.5x', () => {
    expect(backoffMs(1, () => 0)).toBe(5_000)
    expect(backoffMs(1, () => 0.999)).toBeCloseTo(14_990, 0)
    expect(backoffMs(50, () => 0)).toBe(300_000)
  })
})

describe('parseRetryAfter', () => {
  it('reads seconds and HTTP dates', () => {
    expect(parseRetryAfter('30', 0)).toBe(30_000)
    expect(parseRetryAfter('Thu, 01 Jan 1970 00:01:00 GMT', 0)).toBe(60_000)
    expect(parseRetryAfter('Thu, 01 Jan 1970 00:00:00 GMT', 10_000)).toBe(0)
  })
  it('returns null when absent or unreadable', () => {
    expect(parseRetryAfter(null, 0)).toBeNull()
    expect(parseRetryAfter('soon', 0)).toBeNull()
  })
})

const json = (status: number, body: unknown = {}, headers: Record<string, string> = {}) =>
  new Response(JSON.stringify(body), { status, headers })

describe('queue state transitions', () => {
  let db: Db
  let clock: number
  const fetchMock = vi.fn<typeof fetch>()
  const getStatus = vi.fn()
  const onUnauthorized = vi.fn()

  const make = () =>
    createQueue({
      db,
      fetch: fetchMock,
      deviceId: async () => 'device-1',
      getStatus,
      onUnauthorized,
      now: () => clock,
      random: () => 0.5, // jitter factor 1.0
    })
  const post = (shelter_id?: string) => ({ method: 'POST', path: '/v1/things', body: { a: 1 }, volunteer_id: null, shelter_id })

  beforeEach(async () => {
    db = await freshDb()
    clock = 1_000_000
    fetchMock.mockReset()
    getStatus.mockReset()
    onUnauthorized.mockReset()
  })

  it('2xx removes the item and its blob, and sends the idempotency and device headers', async () => {
    fetchMock.mockResolvedValue(json(201))
    const queue = make()
    const item = await queue.enqueue(post())
    await db.put('blobs', { queue_id: item.id })
    await queue.drain()

    expect(await db.count('queue')).toBe(0)
    expect(await db.count('blobs')).toBe(0)
    const [path, init] = fetchMock.mock.calls[0]
    expect(path).toBe('/v1/things')
    expect(init?.headers).toMatchObject({ 'Idempotency-Key': item.id, 'X-Device-Id': 'device-1' })
    expect(init?.body).toBe('{"a":1}')
  })

  it('drains in FIFO order, one at a time', async () => {
    const order: string[] = []
    fetchMock.mockImplementation(async (path) => {
      order.push(String(path))
      return json(200)
    })
    const queue = make()
    for (const n of [1, 2, 3]) await queue.enqueue({ ...post(), path: `/v1/${n}` })
    await queue.drain()
    expect(order).toEqual(['/v1/1', '/v1/2', '/v1/3'])
  })

  it('5xx reschedules with backoff and stops the drain, keeping order', async () => {
    fetchMock.mockResolvedValue(json(500))
    const queue = make()
    await queue.enqueue({ ...post(), path: '/v1/first' })
    await queue.enqueue({ ...post(), path: '/v1/second' })
    await queue.drain()

    const [first, second] = await queue.list()
    expect(first).toMatchObject({ state: 'pending', attempts: 1, next_attempt_at: clock + 10_000 })
    expect(second.attempts).toBe(0)
    expect(fetchMock).toHaveBeenCalledTimes(1)

    // Not due yet: nothing is sent.
    await queue.drain()
    expect(fetchMock).toHaveBeenCalledTimes(1)

    // Due, and it succeeds this time: both go out in order.
    clock += 10_000
    fetchMock.mockResolvedValue(json(200))
    await queue.drain()
    expect(await db.count('queue')).toBe(0)
    expect(fetchMock.mock.calls.map((c) => c[0])).toEqual(['/v1/first', '/v1/first', '/v1/second'])
  })

  it('a network failure also reschedules', async () => {
    fetchMock.mockRejectedValue(new TypeError('offline'))
    const queue = make()
    await queue.enqueue(post())
    await queue.drain()
    expect((await queue.list())[0]).toMatchObject({ state: 'pending', attempts: 1, error: 'network' })
  })

  it('429 respects Retry-After when longer than the backoff', async () => {
    fetchMock.mockResolvedValue(json(429, {}, { 'Retry-After': '120' }))
    const queue = make()
    await queue.enqueue(post())
    await queue.drain()
    expect((await queue.list())[0].next_attempt_at).toBe(clock + 120_000)
  })

  it('503 without unico_degraded is a normal retry', async () => {
    fetchMock.mockResolvedValue(json(503))
    const queue = make()
    await queue.enqueue(post())
    await queue.drain()
    expect((await queue.list())[0]).toMatchObject({ state: 'pending', attempts: 1 })
  })

  it('4xx becomes needs_action with the server error code, and the queue moves on', async () => {
    fetchMock.mockResolvedValueOnce(json(422, { error: 'no_face_detected' })).mockResolvedValue(json(200))
    const queue = make()
    await queue.enqueue({ ...post(), path: '/v1/bad' })
    await queue.enqueue({ ...post(), path: '/v1/good' })
    await queue.drain()
    const items = await queue.list()
    expect(items).toHaveLength(1)
    expect(items[0]).toMatchObject({ path: '/v1/bad', state: 'needs_action', error: 'no_face_detected' })
  })

  it('needs_action items are not sent again', async () => {
    fetchMock.mockResolvedValue(json(400))
    const queue = make()
    await queue.enqueue(post())
    await queue.drain()
    await queue.drain()
    expect(fetchMock).toHaveBeenCalledTimes(1)
  })

  it('401 stops the drain, keeps the item pending and asks for login', async () => {
    fetchMock.mockResolvedValue(json(401))
    const queue = make()
    await queue.enqueue(post())
    await queue.enqueue(post())
    await queue.drain()
    fetchMock.mockClear() // enqueue already triggered drains; measure one clean pass
    onUnauthorized.mockClear()
    await queue.drain()
    expect(onUnauthorized).toHaveBeenCalledTimes(1)
    expect(fetchMock).toHaveBeenCalledTimes(1)
    expect((await queue.list()).map((i) => i.state)).toEqual(['pending', 'pending'])
  })

  it('409 shelter_nearby holds the item and every item of the same shelter', async () => {
    fetchMock.mockResolvedValueOnce(json(409, { error: 'shelter_nearby' })).mockResolvedValue(json(200))
    const queue = make()
    await queue.enqueue(post('s1'))
    await queue.enqueue(post('s1'))
    await queue.enqueue({ ...post('s2'), path: '/v1/other' })
    await queue.drain()
    const items = await queue.list()
    expect(items.map((i) => i.state)).toEqual(['held', 'held'])
    expect(items[0].error).toBe('shelter_nearby')
    expect(fetchMock).toHaveBeenCalledTimes(2) // the 409, then the other shelter's item
  })

  it('holds a dependent item that was already queued before the 409 (spec 9.3)', async () => {
    const queue = make()
    const base = { volunteer_id: 'v1', shelter_id: 's1', attempts: 0, next_attempt_at: 0, state: 'pending' as const }
    await db.put('queue', { ...base, id: 'create', method: 'POST', path: '/v1/shelters', body: {}, created_at: 1 })
    await db.put('queue', { ...base, id: 'address', method: 'PATCH', path: '/v1/shelters/s1', body: {}, created_at: 2 })
    fetchMock.mockResolvedValueOnce(json(409, { error: 'shelter_nearby' })).mockResolvedValue(json(200))
    await queue.drain()
    expect(fetchMock).toHaveBeenCalledTimes(1) // the dependent is never sent
    expect((await queue.list()).map((i) => [i.id, i.state])).toEqual([['create', 'held'], ['address', 'held']])
  })

  it('keeps the conflict detail on the held item', async () => {
    const detail = { shelter_id: 'x', name: 'Ginásio', distance_m: 80, contact: { first_name: 'Rui' } }
    fetchMock.mockResolvedValueOnce(json(409, { error: 'shelter_nearby', detail }))
    const queue = make()
    await queue.enqueue(post('s1'))
    await queue.drain()
    expect((await queue.list())[0]).toMatchObject({ state: 'held', error: 'shelter_nearby', detail })
  })

  it('release amends the creation body, frees the held items and sends them again with the same key', async () => {
    fetchMock.mockResolvedValueOnce(json(409, { error: 'shelter_nearby', detail: {} })).mockResolvedValue(json(201))
    const queue = make()
    const create = await queue.enqueue({ method: 'POST', path: '/v1/shelters', body: { id: 's1', name: 'A' }, volunteer_id: null, shelter_id: 's1' })
    await queue.enqueue({ method: 'PATCH', path: '/v1/shelters/s1', body: { address: {} }, volunteer_id: null, shelter_id: 's1' })
    await queue.drain()
    expect((await queue.list()).map((i) => i.state)).toEqual(['held', 'held'])

    await queue.release('s1', { other_site_reason: 'different building' })
    expect(await db.count('queue')).toBe(0)
    const [, init] = fetchMock.mock.calls[1]
    expect(JSON.parse(init?.body as string)).toEqual({ id: 's1', name: 'A', other_site_reason: 'different building' })
    expect(init?.headers).toMatchObject({ 'Idempotency-Key': create.id })
    expect(JSON.parse(fetchMock.mock.calls[2][1]?.body as string)).toEqual({ address: {} }) // dependent body untouched
  })

  it('release leaves other shelters and non-held items alone', async () => {
    fetchMock.mockResolvedValue(json(500))
    const queue = make()
    await queue.enqueue(post('s2'))
    await queue.release('s1', { x: 1 })
    expect(fetchMock).toHaveBeenCalledTimes(1) // only the normal drain of s2
    expect((await queue.list())[0].body).toEqual({ a: 1 })
  })

  it('onSynced gets the item and the response body after a 2xx (null for 204)', async () => {
    const onSynced = vi.fn()
    fetchMock.mockResolvedValueOnce(json(201, { shelter: { id: 's1' } })).mockResolvedValueOnce(new Response(null, { status: 204 }))
    const queue = createQueue({ db, fetch: fetchMock, deviceId: async () => 'd', getStatus, onSynced })
    await queue.enqueue(post('s1'))
    await queue.enqueue(post('s1'))
    await queue.drain()
    expect(onSynced).toHaveBeenNthCalledWith(1, expect.objectContaining({ shelter_id: 's1' }), { shelter: { id: 's1' } })
    expect(onSynced).toHaveBeenNthCalledWith(2, expect.anything(), null)
  })

  it('503 unico_degraded keeps the item pending until /v1/status is ok again', async () => {
    fetchMock.mockResolvedValueOnce(json(503, { error: 'unico_degraded' })).mockResolvedValue(json(200))
    getStatus.mockResolvedValue({ unico: 'degraded', reason: 'limit' })
    const queue = make()
    await queue.enqueue(post())
    await queue.drain()
    expect((await queue.list())[0]).toMatchObject({ state: 'pending', error: 'unico_degraded', attempts: 0 })

    await queue.drain() // still degraded
    expect(fetchMock).toHaveBeenCalledTimes(1)

    getStatus.mockResolvedValue({ unico: 'ok' })
    await queue.drain()
    expect(fetchMock).toHaveBeenCalledTimes(2)
    expect(await db.count('queue')).toBe(0)
  })

  it('an unreachable /v1/status keeps degraded items waiting', async () => {
    fetchMock.mockResolvedValueOnce(json(503, { error: 'unico_degraded' }))
    getStatus.mockResolvedValue(null)
    const queue = make()
    await queue.enqueue(post())
    await queue.drain()
    await queue.drain()
    expect(fetchMock).toHaveBeenCalledTimes(1)
  })

  it('count includes held and needs_action items', async () => {
    fetchMock.mockResolvedValue(json(400))
    const queue = make()
    await queue.enqueue(post())
    await queue.drain()
    expect(await queue.count()).toBe(1)
  })
})

describe('queue authorization (phase 1)', () => {
  it('sends each item with the session of the volunteer who signed it', async () => {
    const db = await freshDb()
    const fetchMock = vi.fn<typeof fetch>().mockResolvedValue(new Response('{}', { status: 200 }))
    const secrets: Record<string, string> = { ana: 'secret-ana', bia: 'secret-bia' }
    const queue = createQueue({
      db,
      fetch: fetchMock,
      deviceId: async () => 'device-1',
      getStatus: async () => ({ unico: 'ok' }),
      secretFor: async (id) => (id ? (secrets[id] ?? null) : null),
    })
    await queue.enqueue({ method: 'PATCH', path: '/v1/me', body: {}, volunteer_id: 'ana' })
    await queue.enqueue({ method: 'PATCH', path: '/v1/me', body: {}, volunteer_id: 'bia' })
    await queue.enqueue({ method: 'POST', path: '/v1/x', body: {}, volunteer_id: null })
    await queue.drain()
    const headers = fetchMock.mock.calls.map(([, init]) => (init?.headers as Record<string, string>).Authorization)
    expect(headers).toEqual(['Bearer secret-ana', 'Bearer secret-bia', undefined])
  })

  it('a 401 stops the drain and names the volunteer whose session failed', async () => {
    const db = await freshDb()
    const fetchMock = vi.fn<typeof fetch>().mockResolvedValue(new Response('{}', { status: 401 }))
    const onUnauthorized = vi.fn()
    const queue = createQueue({
      db,
      fetch: fetchMock,
      deviceId: async () => 'device-1',
      getStatus: async () => ({ unico: 'ok' }),
      onUnauthorized,
    })
    await queue.enqueue({ method: 'PATCH', path: '/v1/me', body: {}, volunteer_id: 'ana' })
    await queue.drain()
    expect(onUnauthorized).toHaveBeenCalledWith('ana')
    expect(await queue.count()).toBe(1)
  })
})
