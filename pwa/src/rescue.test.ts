import { describe, expect, it, vi } from 'vitest'
import { createQueue } from './queue'
import { distanceText, emptyRescueForm, enqueueRescue, rescueBody, vacancyText, validateRescue } from './rescue'
import { freshDb } from './testing'

const json = (status: number, body: unknown = {}) => new Response(JSON.stringify(body), { status })
const position = { lat: -29.7, lon: -51.1, accuracy: 12 }
const at = new Date('2026-10-21T10:00:00Z')
const jpeg = (...bytes: number[]) => new Blob([new Uint8Array([0xff, 0xd8, 0xff, ...bytes])])
const form = (over: Partial<ReturnType<typeof emptyRescueForm>> = {}) => ({ ...emptyRescueForm(), ...over })

describe('rescue request', () => {
  it('records the point with its accuracy, and leaves out what was not given', () => {
    expect(rescueBody({ id: 'r1', form: form(), position, capturedAt: at })).toEqual({
      id: 'r1', lat: -29.7, lon: -51.1, accuracy: 12, captured_at: '2026-10-21T10:00:00.000Z',
    })
  })

  it('carries name, wristband, note, landing point and the chosen shelter, trimmed', () => {
    const body = rescueBody({ id: 'r1', form: form({ name: ' Rui ', wristband: '', note: ' ferido ', landing_point: ' Praça X ' }), position, capturedAt: at, destination: 's9' })
    expect(body).toMatchObject({ data: { name: 'Rui', note: 'ferido' }, landing_point: 'Praça X', destination_shelter_id: 's9' })
    expect(body.data).not.toHaveProperty('wristband')
  })

  it('needs the point; without a photo it also needs the name or the wristband', () => {
    expect(validateRescue(form(), { position: null, hasPhoto: true })).toEqual(['position'])
    expect(validateRescue(form(), { position, hasPhoto: true })).toEqual([])
    expect(validateRescue(form(), { position, hasPhoto: false })).toEqual(['name_or_wristband'])
    expect(validateRescue(form({ wristband: '0217' }), { position, hasPhoto: false })).toEqual([])
  })
})

describe('rescue in the queue (spec 3.6)', () => {
  const setup = async (getStatus = async () => null as never) => {
    const db = await freshDb()
    const fetchMock = vi.fn<typeof fetch>()
    return { db, fetchMock, queue: createQueue({ db, fetch: fetchMock, deviceId: async () => 'd', getStatus, random: () => 0 }) }
  }

  it('works offline: the photo and the thumbnail wait encrypted, the body has no photo', async () => {
    const { db, fetchMock, queue } = await setup()
    fetchMock.mockRejectedValue(new TypeError('offline'))
    const [item] = await enqueueRescue(queue, { body: rescueBody({ id: 'r1', form: form(), position, capturedAt: at }), photo: jpeg(1), thumbnail: jpeg(2), degraded: false, volunteerId: 'ana' })
    await queue.drain()
    expect(item).toMatchObject({ method: 'POST', path: '/v1/rescues', state: 'pending' })
    expect(JSON.stringify(item.body)).not.toContain('base64')
    expect(await db.count('queue')).toBe(1)
    expect(await db.count('blobs')).toBe(1)
  })

  it('sends the photo and the thumbnail in the body and clears the blob after the 2xx', async () => {
    const { db, fetchMock, queue } = await setup()
    fetchMock.mockResolvedValue(json(201, { rescue_id: 'r1', person_id: 'tok', level: 'assisted' }))
    await enqueueRescue(queue, { body: rescueBody({ id: 'r1', form: form(), position, capturedAt: at }), photo: jpeg(1), thumbnail: jpeg(2), degraded: false, volunteerId: 'ana' })
    await queue.drain()
    const sent = JSON.parse(fetchMock.mock.calls[0]![1]?.body as string)
    expect(sent).toMatchObject({ id: 'r1', lat: -29.7 })
    expect(sent.photo_base64).toBeTruthy()
    expect(sent.thumbnail_base64).toBeTruthy()
    expect(await db.count('blobs')).toBe(0)
  })

  it('drops the thumbnail when there is a name or a wristband (spec 4.6)', async () => {
    const { db, queue } = await setup()
    const [item] = await enqueueRescue(queue, { body: rescueBody({ id: 'r1', form: form({ name: 'Rui' }), position, capturedAt: at }), photo: jpeg(1), thumbnail: jpeg(2), degraded: false, volunteerId: 'ana' })
    const fields = await db.get('blobs', item.id)
    expect(fields).toBeDefined()
    const { decryptPhotoFields } = await import('./crypto')
    expect(Object.keys(await decryptPhotoFields(db, fields!))).toEqual(['photo_base64'])
  })

  it('without a photo there is no blob', async () => {
    const { db, queue } = await setup()
    await enqueueRescue(queue, { body: rescueBody({ id: 'r1', form: form({ name: 'Rui' }), position, capturedAt: at }), degraded: false, volunteerId: 'ana' })
    expect(await db.count('blobs')).toBe(0)
  })

  it('in degraded mode the rescue goes without the photo, which waits as a late biometrics item', async () => {
    const status = vi.fn().mockResolvedValue({ unico: 'degraded', reason: 'limit' })
    const { db, fetchMock, queue } = await setup(status)
    fetchMock.mockImplementation(async (path) => (String(path).endsWith('/biometrics') ? json(503, { error: 'unico_degraded' }) : json(201, { rescue_id: 'r1', person_id: 'r1', level: 'no_biometrics' })))
    const items = await enqueueRescue(queue, { body: rescueBody({ id: 'r1', form: form(), position, capturedAt: at }), photo: jpeg(1), thumbnail: jpeg(2), degraded: true, volunteerId: 'ana' })
    expect(items.map((i) => [i.method, i.path])).toEqual([['POST', '/v1/rescues'], ['PUT', '/v1/people/r1/biometrics']])

    await queue.drain()
    const first = JSON.parse(fetchMock.mock.calls[0]![1]?.body as string)
    expect(first).not.toHaveProperty('photo_base64')
    expect(first.thumbnail_base64).toBeTruthy()
    expect(await db.count('queue')).toBe(1) // the rescue is done, the late biometrics waits
    expect(await db.count('blobs')).toBe(1) // still encrypted

    status.mockResolvedValue({ unico: 'ok' })
    fetchMock.mockResolvedValue(json(200, { person_id: 'tok', level: 'assisted' }))
    await queue.drain()
    expect(JSON.parse(fetchMock.mock.calls.at(-1)![1]?.body as string)).toHaveProperty('photo_base64')
    expect(await db.count('queue')).toBe(0)
    expect(await db.count('blobs')).toBe(0)
  })
})

describe('nearby shelters', () => {
  it('says the vacancies, the full shelter and the shelter without a declared capacity', () => {
    expect(vacancyText({ capacity: 200, occupancy: 142, vacancies: 58 })).toEqual({ key: 'rescue.shelter_vacancies', params: { vacancies: 58, occupancy: 142, capacity: 200 } })
    expect(vacancyText({ capacity: 200, occupancy: 205, vacancies: 0 }).key).toBe('rescue.shelter_full')
    expect(vacancyText({ capacity: null, occupancy: 30, vacancies: null })).toEqual({ key: 'rescue.shelter_no_capacity', params: { occupancy: 30 } })
  })

  it('shows meters under a kilometer and kilometers above', () => {
    expect(distanceText(850.4)).toEqual({ key: 'rescue.distance_m', params: { n: 850 } })
    expect(distanceText(2449)).toEqual({ key: 'rescue.distance_km', params: { n: 2.4 } })
  })
})
