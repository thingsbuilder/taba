import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createApi } from './api'
import type { Db, QueueItem } from './db'
import {
  adminOnDevice,
  approveByAdmin,
  buildDelivery,
  deliveryErrorKey,
  isDelivery,
  loadItems,
  needsOfflineApproval,
  recordDeliveryReply,
  resolveCard,
  selfieAllowed,
  resultText,
  sendWithCapture,
  takeDeliveryReply,
  validateDelivery,
  waitingDeliveries,
} from './delivery'
import { createQueue } from './queue'
import type { LocalSession } from './sessions'
import { freshDb } from './testing'

const json = (status: number, body: unknown = {}) => new Response(JSON.stringify(body), { status })
const SHELTER = 's-1'
const PATH = `/v1/shelters/${SHELTER}/deliveries`
const body = (over = {}) => buildDelivery({ id: 'd-1', cardCode: 'ABCD1234', personId: 'p-1', item: 'mattress', quantity: 1, at: new Date(0), offline: false, ...over })
const session = (id: string, role: 'admin' | 'volunteer'): LocalSession =>
  ({ volunteer_id: id, secret: `secret-${id}`, expires_at: 9e15, ttl_ms: 1, profile: { name: id, phone: null, show_phone: false, language: 'pt-BR' }, global_roles: [], memberships: [{ shelter_id: SHELTER, role }], failed_pins: 0 }) as unknown as LocalSession

describe('request', () => {
  it('carries the client UUID, the card and the person; offline and admin_approval only when set', () => {
    expect(body()).toEqual({ id: 'd-1', card_code: 'ABCD1234', person_id: 'p-1', item: 'mattress', quantity: 1, delivered_at: '1970-01-01T00:00:00.000Z' })
    expect(body({ offline: true, adminApproval: true })).toMatchObject({ offline: true, admin_approval: true })
  })
  it('asks for an item and a whole quantity of at least 1', () => {
    expect(validateDelivery({ item: ' ', quantity: '1' })).toBe('item_required')
    expect(validateDelivery({ item: 'x', quantity: '0' })).toBe('invalid_quantity')
    expect(validateDelivery({ item: 'x', quantity: '1.5' })).toBe('invalid_quantity')
    expect(validateDelivery({ item: 'x', quantity: '2' })).toBeNull()
  })
  it('offers the admin only for a high value item without a selfie', () => {
    expect(needsOfflineApproval({ scope: 'per_person', once_per_event: true, high_value: true }, true)).toBe(true)
    expect(needsOfflineApproval({ scope: 'per_person', once_per_event: true, high_value: true }, false)).toBe(false)
    expect(needsOfflineApproval({ scope: 'per_person', once_per_event: false, high_value: false }, true)).toBe(false)
    expect(needsOfflineApproval(undefined, true)).toBe(false)
  })
  it('finds an admin of this shelter among the sessions on the device', () => {
    expect(adminOnDevice([session('v', 'volunteer')], SHELTER)).toBeNull()
    expect(adminOnDevice([session('v', 'volunteer'), session('a', 'admin')], SHELTER)?.volunteer_id).toBe('a')
    expect(adminOnDevice([session('a', 'admin')], 'other')).toBeNull()
  })
  it('offers no selfie for a person without biometrics (the 428 says admin_approval_only) nor after admin_approval_required', () => {
    expect(selfieAllowed({ error: 'liveness_required', detail: { reason: 'high_value', admin_approval_only: false } })).toBe(true)
    expect(selfieAllowed({ error: 'liveness_required', detail: { reason: 'sample', admin_approval_only: true } })).toBe(false)
    expect(selfieAllowed({ error: 'admin_approval_required', detail: undefined })).toBe(false)
    expect(selfieAllowed({ error: 'revalidation_failed' })).toBe(true)
  })

  it('has a text for every error of the flow', () => {
    expect(deliveryErrorKey('revalidation_failed')).toBe('delivery.error_revalidation_failed')
    expect(deliveryErrorKey('liveness_required')).toBe('error.liveness_required')
    expect(deliveryErrorKey('admin_approval_required')).toBe('error.admin_approval_required')
  })
})

describe('result text', () => {
  it('says registered, or sent to review with the date of the other delivery', () => {
    expect(resultText({ status: 'ok', reasons: [] }).key).toBe('delivery.done')
    const r = resultText({ status: 'in_review', reasons: [{ kind: 'duplicate_in_period', previous_delivered_at: '2026-10-01T12:00:00Z' }] })
    expect(r.key).toBe('delivery.in_review')
    expect(r.params.date).toBeTruthy()
    expect(r.params.shelter).toBeUndefined()
  })
  it('names the other shelter only when the API sent it', () => {
    const r = resultText({ status: 'in_review', reasons: [{ kind: 'duplicate_in_period', previous_delivered_at: '2026-10-01T12:00:00Z', previous_shelter_name: 'Escola' }] })
    expect(r).toMatchObject({ key: 'delivery.in_review_at', params: { shelter: 'Escola' } })
  })
  it('explains a review for lack of a selfie', () => {
    expect(resultText({ status: 'in_review', reasons: [{ kind: 'liveness_unavailable' }] }).key).toBe('delivery.in_review_unavailable')
  })
})

describe('through the queue', () => {
  let db: Db
  let clock: number
  const fetchMock = vi.fn<typeof fetch>()
  const make = () =>
    createQueue({ db, fetch: fetchMock, deviceId: async () => 'dev', getStatus: async () => null, secretFor: async (v) => (v ? `secret-${v}` : null), now: () => clock, random: () => 0.5 })
  const enqueue = (queue: ReturnType<typeof make>, b = body(), volunteer = 'v') =>
    queue.enqueue({ method: 'POST', path: PATH, body: b, volunteer_id: volunteer, shelter_id: SHELTER })

  beforeEach(async () => {
    db = await freshDb()
    clock = 1_000_000
    fetchMock.mockReset()
  })

  it('sends with the item id as Idempotency-Key and keeps the answer for the screen', async () => {
    fetchMock.mockResolvedValue(json(201, { delivery_id: 'd-1', status: 'in_review', reasons: [{ kind: 'duplicate_in_period', previous_delivered_at: '2026-10-01T00:00:00Z' }] }))
    const queue = make()
    const item = await enqueue(queue)
    await queue.drain()
    const [url, init] = fetchMock.mock.calls[0]!
    expect(url).toBe(PATH)
    expect((init!.headers as Record<string, string>)['Idempotency-Key']).toBe(item.id)
    expect(await queue.count()).toBe(0)
    expect((await takeDeliveryReply(db, item.id))?.status).toBe('in_review')
    expect(await takeDeliveryReply(db, item.id)).toBeNull()
  })

  it('does not repeat a 428: the item waits for a person and is found by the screen', async () => {
    fetchMock.mockResolvedValue(json(428, { error: 'liveness_required' }))
    const queue = make()
    const item = await enqueue(queue)
    await queue.drain()
    clock += 3_600_000
    await queue.drain()
    expect(fetchMock).toHaveBeenCalledTimes(1)
    const waiting = waitingDeliveries(await queue.list(), SHELTER)
    expect(waiting.map((w) => [w.id, w.error])).toEqual([[item.id, 'liveness_required']])
    expect(waitingDeliveries(await queue.list(), 'other')).toEqual([])
  })

  it("approval of the admin sends the same item again with admin_approval and the admin's session, same key", async () => {
    fetchMock.mockResolvedValueOnce(json(428, { error: 'liveness_required' })).mockResolvedValueOnce(json(201, { delivery_id: 'd-1', status: 'ok', reasons: [] }))
    const queue = make()
    const item = await enqueue(queue)
    await queue.drain()
    await approveByAdmin(db, queue, item.id, { volunteer_id: 'a' })
    expect(fetchMock).toHaveBeenCalledTimes(2)
    const [, init] = fetchMock.mock.calls[1]!
    const headers = init!.headers as Record<string, string>
    expect(headers['Idempotency-Key']).toBe(item.id)
    expect(headers.Authorization).toBe('Bearer secret-a')
    expect(JSON.parse(init!.body as string)).toMatchObject({ id: 'd-1', admin_approval: true })
    expect(await queue.count()).toBe(0)
  })

  it('a 403 from a volunteer who is not admin keeps the item waiting', async () => {
    fetchMock.mockResolvedValue(json(403, { error: 'admin_approval_required' }))
    const queue = make()
    await enqueue(queue, body({ adminApproval: true }))
    await queue.drain()
    expect(waitingDeliveries(await queue.list(), SHELTER)[0]?.error).toBe('admin_approval_required')
  })

  it('recordDeliveryReply ignores what is not a delivery', async () => {
    const other = { id: 'q', method: 'POST', path: '/v1/things', body: {} } as QueueItem
    await recordDeliveryReply(db, other, { status: 'ok', reasons: [] })
    expect(await takeDeliveryReply(db, 'q')).toBeNull()
    expect(isDelivery(other)).toBe(false)
  })

  // Criterion 54: everything made without signal goes out in the order it was made, and nothing stays behind.
  it('syncs a whole offline day in order, erases the photos and zeroes the counter (criterion 54)', async () => {
    fetchMock.mockRejectedValue(new TypeError('offline'))
    const queue = make()
    const photo = () => ({ photo: new Blob(['face']), thumbnail: new Blob(['thumb']) })
    const sent: [string, string][] = [] // method, path in the order they were made
    const add = async (method: string, path: string, b: unknown, photos?: Parameters<typeof queue.enqueue>[1]) => {
      await queue.enqueue({ method, path, body: b, volunteer_id: 'v', shelter_id: SHELTER }, photos)
      sent.push([method, path])
    }
    await add('POST', '/v1/shelters', { id: SHELTER, name: 'Escola' })
    for (const id of ['p-1', 'p-2', 'p-3']) await add('POST', `/v1/shelters/${SHELTER}/checkins`, { id, capture_id: undefined }, photo())
    await add('POST', `/v1/shelters/${SHELTER}/checkins`, { id: 'p-4', no_biometrics: true, name: 'Ana' })
    await add('POST', `/v1/shelters/${SHELTER}/households`, { id: 'h-1', member_ids: ['p-1', 'p-2'] })
    await add('POST', '/v1/households/h-1/cards', { code: 'ABCD1234' })
    await add('POST', PATH, body({ id: 'd-1' }))
    await add('POST', PATH, body({ id: 'd-2', personId: 'p-2' }))
    await add('POST', '/v1/rescues', { id: 'r-1' }, { photo: new Blob(['face']) })
    await add('POST', '/v1/searches', { id: 'x-1' }, { photos: [new Blob(['a']), new Blob(['b'])] })
    expect(await queue.count()).toBe(sent.length)
    expect((await db.getAll('blobs')).length).toBe(5)

    // The signal is back: the first retry waits out the backoff, then everything drains.
    fetchMock.mockReset()
    fetchMock.mockImplementation(async (url) => json(201, String(url) === PATH ? { delivery_id: 'x', status: 'ok', reasons: [] } : {}))
    clock += 11 * 60_000
    await queue.drain()

    expect(fetchMock.mock.calls.map(([url, init]) => [init!.method, url])).toEqual(sent)
    expect(await queue.count()).toBe(0)
    expect(await db.getAll('blobs')).toEqual([])
  })
})

describe('selfie of a delivery', () => {
  let db: Db
  const fetchMock = vi.fn<typeof fetch>()
  const api = () => createApi(fetchMock, async () => 'dev')
  const sleep = async () => {}
  const queue = { discard: vi.fn(async () => {}) }
  beforeEach(async () => {
    db = await freshDb()
    fetchMock.mockReset()
    queue.discard.mockClear()
    await db.put('captures', { capture_id: 'c-1', created_at: 0, purpose: 'revalidation' })
  })

  it('waits for Unico (202), sends the capture, discards the waiting item and forgets the capture', async () => {
    fetchMock.mockResolvedValueOnce(json(202, { error: 'capture_pending' })).mockResolvedValueOnce(json(201, { delivery_id: 'd-1', status: 'ok', reasons: [] }))
    const res = await sendWithCapture({ db, api: api(), queue }, 'sec', SHELTER, 'c-1', { request: body(), queue_id: 'q-1' }, sleep)
    expect(res).toMatchObject({ ok: true, reply: { status: 'ok' } })
    expect(JSON.parse(fetchMock.mock.calls[1]![1]!.body as string)).toMatchObject({ id: 'd-1', capture_id: 'c-1' })
    expect(queue.discard).toHaveBeenCalledWith('q-1')
    expect(await db.get('captures', 'c-1')).toBeUndefined()
  })

  it('409 revalidation_failed: nothing is recorded, the item stays waiting with that error', async () => {
    const item: QueueItem = { id: 'q-1', method: 'POST', path: PATH, body: body(), volunteer_id: 'v', shelter_id: SHELTER, created_at: 1, attempts: 0, next_attempt_at: 0, state: 'needs_action', error: 'liveness_required' }
    await db.put('queue', item)
    fetchMock.mockResolvedValue(json(409, { error: 'revalidation_failed' }))
    const res = await sendWithCapture({ db, api: api(), queue }, 'sec', SHELTER, 'c-1', { request: body(), queue_id: 'q-1' }, sleep)
    expect(res).toEqual({ ok: false, error: 'revalidation_failed' })
    expect(queue.discard).not.toHaveBeenCalled()
    expect((await db.get('queue', 'q-1'))?.error).toBe('revalidation_failed')
  })

  it('a selfie that never finishes ends as selfie_timeout', async () => {
    fetchMock.mockImplementation(async () => json(202, { error: 'capture_pending' }))
    const res = await sendWithCapture({ db, api: api(), queue }, 'sec', SHELTER, 'c-1', { request: body() }, sleep)
    expect(res).toEqual({ ok: false, error: 'selfie_timeout' })
  })
})

describe('card and items on the phone', () => {
  let db: Db
  beforeEach(async () => {
    db = await freshDb()
  })
  const offlineApi = () => createApi(async () => Promise.reject(new TypeError('offline')), async () => 'dev')

  it('reads the members of a household from the cache without signal', async () => {
    await db.put('people', { id: 'p-1', name: 'Ana' })
    await db.put('people', { id: 'p-2', wristband: 'W7' })
    await db.put('households', { id: 'h-1', member_ids: ['p-1', 'p-2', 'p-3'] })
    await db.put('cards', { code: 'ABCD1234', household_id: 'h-1', status: 'active' })
    const res = await resolveCard(db, offlineApi(), 's', false, 'ABCD1234', '?')
    expect(res).toEqual({ ok: true, householdId: 'h-1', members: [{ id: 'p-1', name: 'Ana', level: undefined }, { id: 'p-2', name: 'W7', level: undefined }, { id: 'p-3', name: '?', level: undefined }] })
  })

  it('a card the phone does not know, or a canceled one, says so', async () => {
    expect(await resolveCard(db, offlineApi(), 's', false, 'ABCD1234', '?')).toEqual({ ok: false, error: 'card.offline_unknown' })
    await db.put('cards', { code: 'ZZZZ0000', household_id: 'h-1', status: 'canceled' })
    expect(await resolveCard(db, offlineApi(), 's', false, 'ZZZZ0000', '?')).toEqual({ ok: false, error: 'card.canceled' })
  })

  it('online it asks the server and keeps the card for later', async () => {
    const fetchMock = vi.fn<typeof fetch>(async () => json(200, { code: 'ABCD1234', status: 'active', household_id: 'h-9', members: [{ person_id: 'p-9', name: 'Bia', level: 'verified' }] }))
    const res = await resolveCard(db, createApi(fetchMock, async () => 'dev'), 's', true, 'ABCD1234', '?')
    expect(res).toMatchObject({ ok: true, householdId: 'h-9', members: [{ id: 'p-9', name: 'Bia' }] })
    expect((await db.get('cards', 'ABCD1234'))?.household_id).toBe('h-9')
  })

  it('items come from the shelter with signal and from the cache without', async () => {
    const items = { mattress: { scope: 'per_person', once_per_event: true, high_value: true } }
    const online = createApi(async () => json(200, { shelter: { id: SHELTER, name: 'E', items } }), async () => 'dev')
    expect(await loadItems(db, online, 's', SHELTER, true)).toEqual(items)
    expect(await loadItems(db, offlineApi(), 's', SHELTER, false)).toEqual(items)
    expect(await loadItems(db, offlineApi(), 's', 'other', false)).toEqual({})
  })
})
