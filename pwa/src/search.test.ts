import { describe, expect, it, vi } from 'vitest'
import { createQueue } from './queue'
import { phase3Label, queueErrorKey } from './queue-errors'
import { canSearch, emptySearchForm, enqueueSearch, reducePhotos, searchBody, shareLink, takeSearchReply, validateSearch } from './search'
import { freshDb } from './testing'

const json = (status: number, body: unknown = {}, headers: Record<string, string> = {}) => new Response(JSON.stringify(body), { status, headers })
const form = { ...emptySearchForm(), name: ' Rui ', nickname: ' ', notes: 'camisa azul', family_name: 'Ana', phone: '+55 11 99999-0000', relationship: 'filha', language: 'en' as const }
const jpeg = (...bytes: number[]) => new Blob([new Uint8Array([0xff, 0xd8, 0xff, ...bytes])])

describe('search form', () => {
  it('needs the missing person, the family and 1 to 5 photos; nickname and notes are optional', () => {
    expect(validateSearch(emptySearchForm(), 0)).toEqual(['name', 'family_name', 'phone', 'relationship', 'photos'])
    expect(validateSearch(form, 1)).toEqual([])
    expect(validateSearch(form, 5)).toEqual([])
    expect(validateSearch(form, 6)).toEqual(['photos'])
  })

  it('builds the body trimmed, without photos and without an empty nickname', () => {
    expect(searchBody('s1', form)).toEqual({
      id: 's1',
      missing_person: { name: 'Rui', notes: 'camisa azul' },
      family: { name: 'Ana', phone: '+55 11 99999-0000', relationship: 'filha', language: 'en' },
    })
  })

  it('searches need a confirmed shelter or a global role of the spec', () => {
    const unconfirmed = [{ shelter_id: 's', name: 'S', role: 'admin' as const, confirmation: 'unconfirmed' as const }]
    expect(canSearch({ memberships: unconfirmed, global_roles: [] })).toBe(false)
    expect(canSearch({ memberships: [{ ...unconfirmed[0]!, confirmation: 'confirmed' }], global_roles: [] })).toBe(true)
    expect(canSearch({ memberships: [], global_roles: ['mediator'] })).toBe(true)
    expect(canSearch({ global_roles: ['validator'] })).toBe(false)
  })
})

describe('search through the queue (spec 3.7, criterion 48)', () => {
  it('keeps the photos encrypted and sends them as photos_base64, then deletes them after the 2xx', async () => {
    const db = await freshDb()
    const fetchMock = vi.fn<typeof fetch>().mockResolvedValue(json(202, { message: 'search_registered', usable_photos: 2 }))
    // Offline: the first drain fails on the network, the photos wait.
    fetchMock.mockRejectedValueOnce(new TypeError('offline'))
    const queue = createQueue({ db, fetch: fetchMock, deviceId: async () => 'd', getStatus: async () => null, random: () => 0 })
    const item = await enqueueSearch(queue, { id: 's1', form, photos: [jpeg(1), jpeg(2), jpeg(3)], volunteerId: 'ana' })
    await queue.drain()

    expect(item.body).not.toHaveProperty('photos_base64')
    expect(await db.count('blobs')).toBe(1)
    const stored = await db.get('blobs', item.id)
    expect(stored?.data).toBeInstanceOf(ArrayBuffer)
    expect(JSON.stringify(item)).not.toContain('/9j/') // no base64 jpeg in the queue item

    await db.put('queue', { ...(await db.get('queue', item.id))!, next_attempt_at: 0 })
    await queue.drain()
    const body = JSON.parse(fetchMock.mock.calls[1]![1]?.body as string)
    expect(fetchMock.mock.calls[1]![0]).toBe('/v1/searches')
    expect(body.photos_base64).toHaveLength(3)
    expect(body).toMatchObject({ id: 's1', missing_person: { name: 'Rui' } })
    expect(await db.count('queue')).toBe(0)
    expect(await db.count('blobs')).toBe(0)
  })

  it('hands the screen usable_photos once, and only for the search that was sent', async () => {
    const db = await freshDb()
    const fetchMock = vi.fn<typeof fetch>().mockResolvedValue(json(202, { message: 'search_registered', usable_photos: 1 }))
    const queue = createQueue({ db, fetch: fetchMock, deviceId: async () => 'd', getStatus: async () => null })
    expect(await takeSearchReply(db, 's1')).toBeNull()
    await enqueueSearch(queue, { id: 's1', form, photos: [jpeg(1)], volunteerId: 'ana' })
    await queue.drain()
    expect(await takeSearchReply(db, 's2')).toBeNull()
    expect(await takeSearchReply(db, 's1')).toBe(1)
    expect(await takeSearchReply(db, 's1')).toBeNull()
  })

  it('429 search_limit is not repeated: the item waits for a person with the photos kept', async () => {
    const db = await freshDb()
    const fetchMock = vi.fn<typeof fetch>().mockResolvedValue(json(429, { error: 'search_limit' }, { 'Retry-After': '5' }))
    const queue = createQueue({ db, fetch: fetchMock, deviceId: async () => 'd', getStatus: async () => null })
    const item = await enqueueSearch(queue, { id: 's1', form, photos: [jpeg(1)], volunteerId: 'ana' })
    await queue.drain()
    await queue.drain()

    expect(fetchMock).toHaveBeenCalledTimes(1)
    expect(await db.get('queue', item.id)).toMatchObject({ state: 'needs_action', error: 'search_limit' })
    expect(await db.count('blobs')).toBe(1)
    await queue.discard(item.id)
    expect(await db.count('blobs')).toBe(0)
  })

  it('a plain 429 still repeats with a wait', async () => {
    const db = await freshDb()
    const fetchMock = vi.fn<typeof fetch>().mockResolvedValue(json(429, { error: 'rate_limited' }))
    const queue = createQueue({ db, fetch: fetchMock, deviceId: async () => 'd', getStatus: async () => null })
    const item = await enqueueSearch(queue, { id: 's1', form, photos: [jpeg(1)], volunteerId: 'ana' })
    await queue.drain()
    expect(await db.get('queue', item.id)).toMatchObject({ state: 'pending', attempts: 1 })
  })
})

it('names the rescue and search items in the pending list, with the daily limit text', () => {
  expect(phase3Label({ method: 'POST', path: '/v1/rescues' })).toBe('rescue.queue_label')
  expect(phase3Label({ method: 'POST', path: '/v1/searches' })).toBe('search.queue_label')
  expect(queueErrorKey('search_limit')).toBe('error.search_limit')
})

describe('reducePhotos', () => {
  const frame = async () => ({ source: {} as CanvasImageSource, width: 10, height: 10 })
  it('reduces each file, in order', async () => {
    const reduce = vi.fn(async (_f: unknown) => jpeg(9))
    const out = await reducePhotos([jpeg(1), jpeg(2)], frame, reduce)
    expect(out).toHaveLength(2)
    expect(reduce).toHaveBeenCalledTimes(2)
  })

  it('is null when a photo cannot get under the limit', async () => {
    const reduce = vi.fn().mockResolvedValueOnce(jpeg(1)).mockResolvedValueOnce(null)
    expect(await reducePhotos([jpeg(1), jpeg(2), jpeg(3)], frame, reduce)).toBeNull()
    expect(reduce).toHaveBeenCalledTimes(2)
  })
})

describe('shareLink', () => {
  it('uses the share sheet when there is one', async () => {
    const share = vi.fn().mockResolvedValue(undefined)
    expect(await shareLink({ share }, 'texto', 'https://x/f/c')).toBe('shared')
    expect(share).toHaveBeenCalledWith({ text: 'texto', url: 'https://x/f/c' })
  })

  it('a closed share sheet is not an error and does not copy', async () => {
    const writeText = vi.fn()
    const share = vi.fn().mockRejectedValue(Object.assign(new Error('x'), { name: 'AbortError' }))
    expect(await shareLink({ share, clipboard: { writeText } }, 't', 'u')).toBe('canceled')
    expect(writeText).not.toHaveBeenCalled()
  })

  it('copies when the share sheet is missing or breaks; says so when it cannot', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined)
    expect(await shareLink({ clipboard: { writeText } }, 't', 'u')).toBe('copied')
    expect(writeText).toHaveBeenCalledWith('t u')
    expect(await shareLink({ share: vi.fn().mockRejectedValue(new Error('x')), clipboard: { writeText } }, 't', 'u')).toBe('copied')
    expect(await shareLink({}, 't', 'u')).toBe('unsupported')
  })
})
