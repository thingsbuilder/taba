import { describe, expect, it, vi } from 'vitest'
import { enqueueDegradedCheckin, isDegraded } from './degraded'
import { createQueue } from './queue'
import { freshDb } from './testing'

const json = (status: number, body: unknown = {}) => new Response(JSON.stringify(body), { status })
const checkin = { id: 'p1', data: { is_child: false, name: 'Rui' }, legal_basis: 'consent' as const, client_created_at: '2026-10-21T10:00:00Z' }
const who = { volunteer_id: 'ana', shelter_id: 's1' }

it('knows when the Unico is out', () => {
  expect(isDegraded({ unico: 'degraded', reason: 'limit' })).toBe(true)
  expect(isDegraded({ unico: 'ok' })).toBe(false)
  expect(isDegraded(null)).toBe(false) // unreachable status is "offline", not "degraded"
})

describe('check-in in degraded mode (spec 5.7)', () => {
  it('registers by path C at once; the photo waits encrypted for the status to be ok', async () => {
    const db = await freshDb()
    const fetchMock = vi.fn<typeof fetch>()
    const status = vi.fn().mockResolvedValue({ unico: 'degraded', reason: 'limit' })
    const queue = createQueue({ db, fetch: fetchMock, deviceId: async () => 'd', getStatus: status })
    fetchMock.mockImplementation(async (path) => (String(path).endsWith('/biometrics') ? json(503, { error: 'unico_degraded' }) : json(201, { person_id: 'p1', level: 'no_biometrics' })))

    const [first, late] = await enqueueDegradedCheckin(queue, checkin, new Blob([new Uint8Array([0xff, 0xd8, 0xff])]), who)
    expect(first.body).toMatchObject({ id: 'p1', no_biometrics: true })
    expect(first.body).not.toHaveProperty('photo_base64')
    expect(late).toMatchObject({ method: 'PUT', path: '/v1/people/p1/biometrics' })

    await queue.drain()
    const sent = fetchMock.mock.calls.map(([path]) => path)
    expect(sent).toEqual(['/v1/shelters/s1/checkins', '/v1/people/p1/biometrics']) // the registration went out; the photo got the 503
    expect(await db.count('blobs')).toBe(1) // still encrypted in the queue

    await queue.drain()
    expect(fetchMock).toHaveBeenCalledTimes(2) // status still degraded: no new call

    status.mockResolvedValue({ unico: 'ok' })
    fetchMock.mockResolvedValue(json(200, { person_id: 'token-1', level: 'assisted' }))
    await queue.drain()
    expect(JSON.parse(fetchMock.mock.calls[2][1]?.body as string)).toHaveProperty('photo_base64')
    expect(await db.count('queue')).toBe(0)
    expect(await db.count('blobs')).toBe(0)
  })

  it('without a photo there is only the registration', async () => {
    const db = await freshDb()
    const queue = createQueue({ db, fetch: vi.fn<typeof fetch>().mockResolvedValue(json(500)), deviceId: async () => 'd', getStatus: async () => null })
    expect(await enqueueDegradedCheckin(queue, checkin, undefined, who)).toHaveLength(1)
  })
})

describe('a photo check-in queued before Unico went out (spec 5.7, 9.2)', () => {
  const JPEG = new Uint8Array([0xff, 0xd8, 0xff, 0xe0, 1, 2])
  const photoCheckin = { method: 'POST', path: '/v1/shelters/s1/checkins', body: { id: 'p1', data: { is_child: false, name: 'Rui' }, legal_basis: 'consent' }, volunteer_id: 'ana', shelter_id: 's1' }

  it.each(['unico_degraded', 'unico_limit'])('the 503 %s turns it into path C and a late biometrics item that keeps the photo encrypted', async (code) => {
    const db = await freshDb()
    const fetchMock = vi.fn<typeof fetch>()
    const status = vi.fn().mockResolvedValue({ unico: 'degraded', reason: 'limit' })
    const queue = createQueue({ db, fetch: fetchMock, deviceId: async () => 'd', getStatus: status })
    const thumbnail = new Blob([new Uint8Array([1, 2, 3])])
    fetchMock
      .mockResolvedValueOnce(json(503, { error: code })) // the photo check-in, refused: Unico is out
      .mockResolvedValueOnce(json(201, { person_id: 'p1', level: 'no_biometrics', is_new: true, history: {} })) // path C
      .mockResolvedValue(json(503, { error: 'unico_degraded' }))
    const item = await queue.enqueue(photoCheckin, { photo: new Blob([JPEG]), thumbnail })
    await queue.drain()
    await queue.drain()

    const bodies = fetchMock.mock.calls.map(([path, init]) => [path, JSON.parse(String(init?.body ?? 'null'))] as const)
    expect(bodies[0]![1]).toHaveProperty('photo_base64') // as first sent
    expect(bodies[1]![0]).toBe('/v1/shelters/s1/checkins')
    expect(bodies[1]![1]).toMatchObject({ id: 'p1', no_biometrics: true, thumbnail_base64: 'AQID' })
    expect(bodies[1]![1]).not.toHaveProperty('photo_base64')
    // The registration went out (same key as before); what is left is the late biometrics with the photo.
    expect(fetchMock.mock.calls[1]![1]?.headers).toMatchObject({ 'Idempotency-Key': item.id })
    const left = await queue.list()
    expect(left.map((i) => `${i.method} ${i.path} ${i.state}`)).toEqual(['PUT /v1/people/p1/biometrics pending'])
    const stored = new Uint8Array((await db.get('blobs', left[0]!.id))!.data)
    expect(Array.from(stored.subarray(0, 3))).not.toEqual([0xff, 0xd8, 0xff])
    expect(await db.count('blobs')).toBe(1)

    // Back to normal: the photo goes out as late biometrics.
    status.mockResolvedValue({ unico: 'ok' })
    fetchMock.mockResolvedValue(json(200, { person_id: 'token-1', level: 'assisted' }))
    await queue.drain()
    const last = fetchMock.mock.calls.at(-1)!
    expect(last[0]).toBe('/v1/people/p1/biometrics')
    expect(JSON.parse(String(last[1]?.body))).toHaveProperty('photo_base64')
    expect(await db.count('queue')).toBe(0)
    expect(await db.count('blobs')).toBe(0)
  })

  it('a check-in with a selfie capture is not turned into path C: it waits for the status', async () => {
    const db = await freshDb()
    const fetchMock = vi.fn<typeof fetch>().mockResolvedValue(json(503, { error: 'unico_degraded' }))
    const queue = createQueue({ db, fetch: fetchMock, deviceId: async () => 'd', getStatus: async () => ({ unico: 'degraded', reason: 'limit' }) })
    await queue.enqueue({ ...photoCheckin, body: { id: 'p1', capture_id: 'c1', data: { is_child: false } } })
    await queue.drain()
    const [only] = await queue.list()
    expect(only).toMatchObject({ method: 'POST', state: 'pending', error: 'unico_degraded' })
    expect(JSON.parse(String(fetchMock.mock.calls[0]![1]?.body))).not.toHaveProperty('no_biometrics')
    await queue.drain()
    expect(fetchMock).toHaveBeenCalledTimes(1) // the status says degraded: no new call
  })
})
