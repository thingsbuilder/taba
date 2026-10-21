import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { Db, QueueItem } from './db'
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
    const item = await queue.enqueue(post(), { photo: new Blob([new Uint8Array([0xff, 0xd8, 0xff, 1])]) })
    expect(await db.count('blobs')).toBe(1)
    await queue.drain()

    expect(await db.count('queue')).toBe(0)
    expect(await db.count('blobs')).toBe(0)
    const [path, init] = fetchMock.mock.calls[0]
    expect(path).toBe('/v1/things')
    expect(init?.headers).toMatchObject({ 'Idempotency-Key': item.id, 'X-Device-Id': 'device-1' })
    expect(JSON.parse(init?.body as string)).toEqual({ a: 1, photo_base64: '/9j/AQ==' })
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

const JPEG = new Uint8Array([0xff, 0xd8, 0xff, 0xe0, 0, 0x10, 0x4a, 0x46, 0x49, 0x46, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9])

describe('encrypted photos in the queue (spec 6.1.7)', () => {
  let db: Db
  const fetchMock = vi.fn<typeof fetch>()
  const make = () =>
    createQueue({ db, fetch: fetchMock, deviceId: async () => 'd', getStatus: async () => ({ unico: 'ok' }), random: () => 0.5 })
  const checkin = { method: 'POST', path: '/v1/shelters/s1/checkins', body: { id: 'p1', data: { is_child: false } }, volunteer_id: 'ana', shelter_id: 's1' }
  const due = async (id: string) => db.put('queue', { ...(await db.get('queue', id))!, next_attempt_at: 0 })

  beforeEach(async () => {
    db = await freshDb()
    fetchMock.mockReset()
  })

  it('the stored blob is not a readable JPEG and the photo is not in the queue item', async () => {
    fetchMock.mockResolvedValue(json(500))
    const queue = make()
    const item = await queue.enqueue(checkin, { photo: new Blob([JPEG]) })
    const stored = new Uint8Array((await db.get('blobs', item.id))!.data)
    expect(Array.from(stored.subarray(0, 3))).not.toEqual([0xff, 0xd8, 0xff])
    expect(JSON.stringify(await db.get('queue', item.id))).not.toContain('photo_base64')
  })

  it('goes out as photo_base64 and thumbnail_base64; the blob survives a 5xx and goes with the 2xx', async () => {
    fetchMock.mockResolvedValueOnce(json(500)).mockResolvedValue(json(201, { person_id: 'p1', level: 'assisted' }))
    const queue = make()
    const item = await queue.enqueue(checkin, { photo: new Blob([JPEG]), thumbnail: new Blob([new Uint8Array([1, 2, 3])]) })
    await queue.drain()
    expect(await db.count('blobs')).toBe(1)
    await due(item.id)
    await queue.drain()
    const sent = JSON.parse(fetchMock.mock.calls[1][1]?.body as string)
    expect(sent).toMatchObject({ id: 'p1', thumbnail_base64: 'AQID' })
    expect(Uint8Array.from(atob(sent.photo_base64), (c) => c.charCodeAt(0))).toEqual(JPEG)
    expect(await db.count('blobs')).toBe(0)
    expect(await db.count('queue')).toBe(0)
  })

  it('a photo that cannot be decrypted becomes needs_action and is not sent', async () => {
    fetchMock.mockResolvedValue(json(500))
    const queue = make()
    const item = await queue.enqueue(checkin, { photo: new Blob([JPEG]) })
    await queue.drain()
    await db.delete('config', 'photo_key')
    await due(item.id)
    fetchMock.mockClear()
    await queue.drain()
    expect(fetchMock).not.toHaveBeenCalled()
    expect((await queue.list())[0]).toMatchObject({ state: 'needs_action', error: 'photo_unreadable' })
  })

  it('criterion 55: a resend (answer lost) uses the same Idempotency-Key and body, so the server answers from memory', async () => {
    fetchMock.mockRejectedValueOnce(new TypeError('answer lost')).mockResolvedValue(json(201, { person_id: 'p1' }))
    const queue = make()
    const item = await queue.enqueue(checkin, { photo: new Blob([JPEG]) })
    await queue.drain()
    await due(item.id)
    await queue.drain()
    const [first, second] = fetchMock.mock.calls.map(([, init]) => init)
    expect((first?.headers as Record<string, string>)['Idempotency-Key']).toBe(item.id)
    expect((second?.headers as Record<string, string>)['Idempotency-Key']).toBe(item.id)
    expect(second?.body).toBe(first?.body)
    expect(await db.count('queue')).toBe(0)
  })
})

describe('taking over pending items (spec 9.2, criterion 56)', () => {
  const secrets: Record<string, string> = { bia: 'secret-bia' } // ana's session is gone
  const make = (db: Db, fetchMock: typeof fetch) =>
    createQueue({
      db,
      fetch: fetchMock,
      deviceId: async () => 'd',
      getStatus: async () => ({ unico: 'ok' }),
      secretFor: async (id) => (id ? (secrets[id] ?? null) : null),
      onUnauthorized: () => undefined,
    })
  const item = { method: 'POST', path: '/v1/shelters/s1/checkins', body: { id: 'p1' }, volunteer_id: 'ana', shelter_id: 's1' }

  it('an item of another volunteer is never sent with the session of the active one', async () => {
    const db = await freshDb()
    const fetchMock = vi.fn<typeof fetch>().mockResolvedValue(json(401))
    const queue = make(db, fetchMock)
    await queue.enqueue(item)
    await queue.drain()
    for (const [, init] of fetchMock.mock.calls) expect((init?.headers as Record<string, string>).Authorization).toBeUndefined()
    expect((await queue.list())[0]).toMatchObject({ volunteer_id: 'ana', state: 'pending' })
  })

  it('after the admin takes over, it goes with the admin session and says whose it was', async () => {
    const db = await freshDb()
    const fetchMock = vi.fn<typeof fetch>().mockResolvedValue(json(401))
    const queue = make(db, fetchMock)
    const queued = await queue.enqueue(item)
    await queue.enqueue({ ...item, shelter_id: 's2', path: '/v1/shelters/s2/checkins' })
    await queue.enqueue({ ...item, volunteer_id: 'cris' })
    await queue.drain()

    fetchMock.mockReset()
    fetchMock.mockImplementation(async (_p, init) => json((init?.headers as Record<string, string>).Authorization ? 201 : 401))
    expect(await queue.takeOver('ana', 'bia', ['s1'])).toBe(1) // only the shelter where bia is admin
    const [path, init] = fetchMock.mock.calls[0]
    expect(path).toBe('/v1/shelters/s1/checkins')
    expect(init?.headers).toMatchObject({ Authorization: 'Bearer secret-bia', 'X-Taken-Over-From': 'ana', 'Idempotency-Key': queued.id })
    const left = await queue.list()
    expect(left.map((i) => [i.volunteer_id, i.shelter_id])).toEqual([['ana', 's2'], ['cris', 's1']])
  })

  it('keeps the first author when an item is taken over twice', async () => {
    const db = await freshDb()
    const fetchMock = vi.fn<typeof fetch>().mockResolvedValue(json(500))
    const queue = make(db, fetchMock)
    await queue.enqueue(item)
    await queue.drain()
    await queue.takeOver('ana', 'bia', ['s1'])
    await queue.takeOver('bia', 'dani', ['s1'])
    expect((await queue.list())[0]).toMatchObject({ volunteer_id: 'dani', taken_over_from: 'ana' })
  })
})

describe('conflicts of spec 9.4 in the queue', () => {
  let db: Db
  const fetchMock = vi.fn<typeof fetch>()
  const make = () => createQueue({ db, fetch: fetchMock, deviceId: async () => 'd', getStatus: async () => ({ unico: 'ok' }) })
  const queued = (id: string, createdAt: number, over: Partial<QueueItem>): QueueItem => ({
    id, method: 'POST', path: '/v1/x', body: {}, volunteer_id: 'ana', shelter_id: 's1', created_at: createdAt, attempts: 0, next_attempt_at: 0, state: 'pending', ...over,
  })
  const sentIds = () => fetchMock.mock.calls.map(([, init]) => JSON.parse(init?.body as string).id)

  beforeEach(async () => {
    db = await freshDb()
    fetchMock.mockReset()
  })

  it.each(['person_in_other_household', 'household_has_card'])('409 %s needs a person, keeps the detail and the queue moves on', async (code) => {
    fetchMock.mockResolvedValueOnce(json(409, { error: code, detail: { household_id: 'h9' } })).mockResolvedValue(json(200))
    const queue = make()
    await queue.enqueue({ method: 'POST', path: '/v1/shelters/s1/households', body: { id: 'h1' }, volunteer_id: 'ana', shelter_id: 's1' })
    await queue.enqueue({ method: 'PATCH', path: '/v1/people/p2', body: {}, volunteer_id: 'ana', shelter_id: 's1' })
    await queue.drain()
    const items = await queue.list()
    expect(items).toHaveLength(1)
    expect(items[0]).toMatchObject({ state: 'needs_action', error: code, detail: { household_id: 'h9' } })
  })

  it('retry sends the item again with the same key; discard drops it and its photo', async () => {
    fetchMock.mockResolvedValueOnce(json(409, { error: 'household_has_card' })).mockResolvedValue(json(201))
    const queue = make()
    const a = await queue.enqueue({ method: 'POST', path: '/v1/households/h1/cards', body: { code: 'C' }, volunteer_id: 'ana' })
    await queue.drain()
    await queue.retry(a.id)
    expect(fetchMock.mock.calls.map(([, init]) => (init?.headers as Record<string, string>)['Idempotency-Key'])).toEqual([a.id, a.id])

    fetchMock.mockResolvedValue(json(409, { error: 'person_in_other_household' }))
    const b = await queue.enqueue({ method: 'POST', path: '/v1/shelters/s1/households', body: { id: 'h2' }, volunteer_id: 'ana' }, { photo: new Blob([JPEG]) })
    await queue.drain()
    await queue.discard(b.id)
    expect(await db.count('queue')).toBe(0)
    expect(await db.count('blobs')).toBe(0)
  })

  it('an item that waits for GET /status holds back what mentions its person, not the rest', async () => {
    const status = vi.fn().mockResolvedValue({ unico: 'degraded', reason: 'limit' })
    const queue = createQueue({ db, fetch: fetchMock, deviceId: async () => 'd', getStatus: status })
    await db.put('queue', queued('a', 1, { path: '/v1/shelters/s1/checkins', body: { id: 'p1' }, error: 'unico_degraded' }))
    await db.put('queue', queued('b', 2, { path: '/v1/shelters/s1/households', body: { id: 'h1', member_ids: ['p1'] } }))
    await db.put('queue', queued('c', 3, { path: '/v1/shelters/s1/households', body: { id: 'h2', member_ids: ['p9'] } }))
    fetchMock.mockResolvedValue(json(201))
    await queue.drain()
    expect(sentIds()).toEqual(['h2'])

    status.mockResolvedValue({ unico: 'ok' })
    await queue.drain()
    expect(sentIds()).toEqual(['h2', 'p1', 'h1'])
    expect(await db.count('queue')).toBe(0)
  })

  it('when the server derives another person id, queued items and caches follow it (spec 4.12)', async () => {
    await db.put('people', { id: 'p1', name: 'Rui', pending: true })
    await db.put('households', { id: 'h1', member_ids: ['p1', 'p2'], head_id: 'p1', pending: true })
    await db.put('queue', queued('a', 1, { path: '/v1/shelters/s1/checkins', body: { id: 'p1' } }))
    await db.put('queue', queued('b', 2, { path: '/v1/shelters/s1/households', body: { id: 'h1', member_ids: ['p1', 'p2'], head_id: 'p1' } }))
    await db.put('queue', queued('c', 3, { method: 'PATCH', path: '/v1/people/p1', body: { id: 'x' } }))
    fetchMock.mockResolvedValueOnce(json(201, { person_id: 'token-1', level: 'assisted', is_new: true, history: {} })).mockResolvedValue(json(200))
    await make().drain()
    expect(JSON.parse(fetchMock.mock.calls[1][1]?.body as string)).toEqual({ id: 'h1', member_ids: ['token-1', 'p2'], head_id: 'token-1' })
    expect(fetchMock.mock.calls[2][0]).toBe('/v1/people/token-1')
    expect(await db.get('people', 'p1')).toBeUndefined()
    expect(await db.get('people', 'token-1')).toMatchObject({ name: 'Rui', pending: false, level: 'assisted' })
    expect(await db.get('households', 'h1')).toMatchObject({ member_ids: ['token-1', 'p2'], head_id: 'token-1' })
  })

  it('an older edit that the server ignored brings back the current state (spec 9.4)', async () => {
    await db.put('people', { id: 'p1', name: 'Typed offline', pending: true })
    fetchMock.mockResolvedValue(json(200, { person: { id: 'p1', name: 'Newer name', level: 'verified' } }))
    await createQueue({ db, fetch: fetchMock, deviceId: async () => 'd', getStatus: async () => ({ unico: 'ok' }) }).enqueue({
      method: 'PATCH', path: '/v1/people/p1', body: { data: { name: 'Typed offline' }, client_edited_at: 'x' }, volunteer_id: 'ana', shelter_id: 's1',
    })
    await vi.waitFor(async () => expect(await db.count('queue')).toBe(0))
    expect(await db.get('people', 'p1')).toMatchObject({ name: 'Newer name', level: 'verified', pending: false })
  })
})
