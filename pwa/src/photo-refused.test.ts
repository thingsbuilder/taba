import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { Db } from './db'
import { decryptPhotoFields } from './crypto'
import { createQueue } from './queue'
import { MAX_PHOTO_ATTEMPTS, photoRefusalActions } from './queue-errors'
import { freshDb } from './testing'

// A photo Unico refuses will be refused again: the item waits for a person with all its data (spec 5.3).

const json = (status: number, body: unknown = {}) => new Response(JSON.stringify(body), { status })
const JPEG = new Uint8Array([0xff, 0xd8, 0xff, 0xe0, 1, 2])
const checkin = {
  method: 'POST', path: '/v1/shelters/s1/checkins', volunteer_id: 'ana', shelter_id: 's1',
  body: { id: 'p1', data: { is_child: false, name: 'Rui', wristband: 'B7' }, legal_basis: 'consent', client_created_at: 'x' },
}

describe('a refused photo in the queue', () => {
  let db: Db
  const fetchMock = vi.fn<typeof fetch>()
  const made: ReturnType<typeof createQueue>[] = []
  const make = () => {
    const queue = createQueue({ db, fetch: fetchMock, deviceId: async () => 'd', getStatus: async () => ({ unico: 'ok' }), random: () => 0.5 })
    made.push(queue)
    return queue
  }
  beforeEach(async () => {
    db = await freshDb()
    fetchMock.mockReset()
  })

  it.each(['photo_rejected', 'no_face', 'invalid_image'])('%s is never sent again, and the data waits with the photo', async (code) => {
    fetchMock.mockImplementation(async () => json(422, { error: code }))
    const queue = make()
    const item = await queue.enqueue(checkin, { photo: new Blob([JPEG]), thumbnail: new Blob([new Uint8Array([1])]) })
    await queue.drain()
    await queue.drain()
    await queue.drain()
    expect(fetchMock).toHaveBeenCalledTimes(1)
    const [waiting] = await queue.list()
    expect(waiting).toMatchObject({ id: item.id, state: 'needs_action', error: code, photo_refusals: 1, body: checkin.body })
    expect(await db.count('blobs')).toBe(1)
    expect(photoRefusalActions(waiting!)).toEqual({ again: true })
  })

  it('a new photo goes out again with the same data and key; after two refusals only "without a photo" is left', async () => {
    fetchMock.mockImplementation(async () => json(422, { error: 'photo_rejected' }))
    const queue = make()
    const item = await queue.enqueue(checkin, { photo: new Blob([JPEG]) })
    await queue.drain()
    await queue.replacePhoto(item.id, new Blob([new Uint8Array([0xff, 0xd8, 9, 9])]))
    expect(fetchMock).toHaveBeenCalledTimes(2)
    const second = JSON.parse(String(fetchMock.mock.calls[1]![1]?.body))
    expect(second).toMatchObject({ id: 'p1', data: { name: 'Rui', wristband: 'B7' } })
    expect(Uint8Array.from(atob(second.photo_base64), (c) => c.charCodeAt(0))).toEqual(new Uint8Array([0xff, 0xd8, 9, 9]))
    expect(fetchMock.mock.calls[1]![1]?.headers).toMatchObject({ 'Idempotency-Key': item.id })

    const [twice] = await queue.list()
    expect(twice).toMatchObject({ state: 'needs_action', photo_refusals: MAX_PHOTO_ATTEMPTS })
    expect(photoRefusalActions(twice!)).toEqual({ again: false })
  })

  it('without a photo the same data is registered by path C, keeping the thumbnail, and the photo is gone', async () => {
    fetchMock.mockResolvedValueOnce(json(422, { error: 'photo_rejected' })).mockImplementation(async () => json(201, { person_id: 'p1', level: 'no_biometrics', is_new: true, history: {} }))
    const queue = make()
    const item = await queue.enqueue({ ...checkin, body: { ...checkin.body, data: { is_child: true, child_status: 'unaccompanied' } } }, { photo: new Blob([JPEG]), thumbnail: new Blob([new Uint8Array([1, 2, 3])]) })
    await queue.drain()
    await queue.registerWithoutPhoto(item.id)
    const sent = JSON.parse(String(fetchMock.mock.calls[1]![1]?.body))
    expect(sent).toMatchObject({ id: 'p1', no_biometrics: true, thumbnail_base64: 'AQID', data: { child_status: 'unaccompanied' } })
    expect(sent).not.toHaveProperty('photo_base64')
    expect(await db.count('queue')).toBe(0)
    expect(await db.count('blobs')).toBe(0)
  })

  it('a refused photo of a late biometrics item is dropped: the person stays as registered', async () => {
    fetchMock.mockImplementation(async () => json(422, { error: 'photo_rejected' }))
    const queue = make()
    const item = await queue.enqueue({ method: 'PUT', path: '/v1/people/p1/biometrics', body: {}, volunteer_id: 'ana', shelter_id: 's1' }, { photo: new Blob([JPEG]) })
    await queue.drain()
    expect(photoRefusalActions((await queue.list())[0]!)).toEqual({ again: true })
    await queue.registerWithoutPhoto(item.id)
    expect(await db.count('queue')).toBe(0)
    expect(await db.count('blobs')).toBe(0)
  })

  it('the photo stays readable for the volunteer until a decision (it is still encrypted)', async () => {
    fetchMock.mockImplementation(async () => json(422, { error: 'photo_rejected' }))
    const queue = make()
    const item = await queue.enqueue(checkin, { photo: new Blob([JPEG]) })
    await queue.drain()
    const stored = (await db.get('blobs', item.id))!
    expect(Array.from(new Uint8Array(stored.data).subarray(0, 3))).not.toEqual([0xff, 0xd8, 0xff])
    expect((await decryptPhotoFields(db, stored)).photo_base64).toBeTruthy()
  })

  it('offers the ways out only for a refused photo of a photo item', () => {
    const item = { method: 'POST', path: '/v1/shelters/s1/checkins', state: 'needs_action' as const, error: 'photo_rejected' }
    expect(photoRefusalActions(item)).toEqual({ again: true })
    expect(photoRefusalActions({ ...item, error: 'person_in_other_household' })).toBeNull()
    expect(photoRefusalActions({ ...item, state: 'pending' as never })).toBeNull()
    expect(photoRefusalActions({ ...item, path: '/v1/shelters/s1/exits' })).toBeNull()
    expect(photoRefusalActions({ ...item, photo_refusals: 5 })).toEqual({ again: false })
  })
})
