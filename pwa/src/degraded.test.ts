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
