import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { Db } from './db'
import { cancelCard, createHousehold, createSoloHousehold, findLocalCard, householdOf, issueCard, updateHousehold } from './households'
import { createQueue } from './queue'
import { freshDb } from './testing'

const who = { volunteer_id: 'ana', shelter_id: 's1' }
const json = (status: number, body: unknown = {}) => new Response(JSON.stringify(body), { status })

describe('households and cards offline', () => {
  let db: Db
  const fetchMock = vi.fn<typeof fetch>()
  const make = () => createQueue({ db, fetch: fetchMock, deviceId: async () => 'd', getStatus: async () => null })

  beforeEach(async () => {
    db = await freshDb()
    fetchMock.mockReset()
    fetchMock.mockRejectedValue(new TypeError('offline'))
    await db.put('people', { id: 'p1', name: 'Rui', current_shelter_id: 's1' })
    await db.put('people', { id: 'p2', name: 'Lia', current_shelter_id: 's1' })
  })

  it('creates the household in the cache and queues it for the shelter', async () => {
    const queue = make()
    await createHousehold(db, queue, who, { id: 'h1', member_ids: ['p1', 'p2'], head_id: 'p1', pets: [{ species: 'dog', count: 1 }] })
    expect(await db.get('households', 'h1')).toMatchObject({ member_ids: ['p1', 'p2'], head_id: 'p1', pending: true })
    expect((await db.get('people', 'p2'))?.household_id).toBe('h1')
    const [item] = await queue.list()
    expect(item).toMatchObject({ method: 'POST', path: '/v1/shelters/s1/households', shelter_id: 's1', volunteer_id: 'ana' })
    expect(item.body).toMatchObject({ id: 'h1', member_ids: ['p1', 'p2'], head_id: 'p1', pets: [{ species: 'dog', count: 1 }] })
  })

  it('makes a household of one person after the check-in, with the person as head, in the queue after the check-in', async () => {
    const queue = make()
    await queue.enqueue({ method: 'POST', path: '/v1/shelters/s1/checkins', body: { id: 'p1' }, ...who })
    const id = await createSoloHousehold(db, queue, who, 'p1')
    expect(await db.get('households', id)).toMatchObject({ member_ids: ['p1'], head_id: 'p1', pending: true })
    expect((await db.get('people', 'p1'))?.household_id).toBe(id)
    const items = await queue.list()
    expect(items.map((i) => i.path)).toEqual(['/v1/shelters/s1/checkins', '/v1/shelters/s1/households'])
    expect(items[1]!.body).toMatchObject({ id, member_ids: ['p1'], head_id: 'p1' })
  })

  it('offers the options only to a person with no household, on the server or on this phone', () => {
    expect(householdOf({})).toBeUndefined()
    expect(householdOf({}, {})).toBeUndefined()
    expect(householdOf({ household_id: 'h1' })).toBe('h1')
    expect(householdOf({}, { household_id: 'h2' })).toBe('h2')
  })

  it('changes members, head and pets; removing the head clears it', async () => {
    const queue = make()
    await createHousehold(db, queue, who, { id: 'h1', member_ids: ['p1'], head_id: 'p1' })
    await updateHousehold(db, queue, who, 'h1', { add_member_ids: ['p2'], head_id: 'p2' })
    expect(await db.get('households', 'h1')).toMatchObject({ member_ids: ['p1', 'p2'], head_id: 'p2' })
    await updateHousehold(db, queue, who, 'h1', { remove_member_ids: ['p2'], pets: [{ species: 'cat', count: 2 }] })
    expect(await db.get('households', 'h1')).toMatchObject({ member_ids: ['p1'], head_id: undefined, pets: [{ species: 'cat', count: 2 }] })
    expect((await db.get('people', 'p2'))?.household_id).toBeUndefined()
    expect((await queue.list()).map((i) => i.method)).toEqual(['POST', 'PATCH', 'PATCH'])
  })

  it('issues a card with an 8-character code that can be read back with no signal', async () => {
    const queue = make()
    const household = await createHousehold(db, queue, who, { id: 'h1', member_ids: ['p1'] })
    const code = await issueCard(db, queue, who, household)
    expect(code).toMatch(/^[0-9A-HJKMNP-TV-Z]{8}$/)
    const found = await findLocalCard(db, code)
    expect(found?.household?.id).toBe('h1')
    expect(found?.household?.card_code).toBe(code)
    expect(await findLocalCard(db, 'ZZZZZZZZ')).toBeNull()
  })

  it('cancel with a substitute queues the cancellation first, then the new card', async () => {
    const queue = make()
    const household = await createHousehold(db, queue, who, { id: 'h1', member_ids: ['p1'] })
    const old = await issueCard(db, queue, who, household, 'ABCD1234')
    const fresh = await cancelCard(db, queue, who, (await db.get('households', 'h1'))!, old, true)
    expect(fresh).not.toBe(old)
    expect(await db.get('cards', old)).toMatchObject({ status: 'canceled' })
    expect(await db.get('cards', fresh!)).toMatchObject({ status: 'active' })
    const items = (await queue.list()).slice(1)
    expect(items.map((i) => [i.method, i.path])).toEqual([
      ['POST', '/v1/households/h1/cards'],
      ['POST', '/v1/cards/ABCD1234/cancel'],
      ['POST', '/v1/households/h1/cards'],
    ])
    expect(items[1].body).toEqual({ replaced_by: fresh })
    expect(items[2].body).toMatchObject({ code: fresh })
  })

  it('every card issue, first or substitute, sends code and an ISO client_issued_at', async () => {
    const queue = make()
    const household = await createHousehold(db, queue, who, { id: 'h1', member_ids: ['p1'] })
    const old = await issueCard(db, queue, who, household, 'ABCD1234')
    await cancelCard(db, queue, who, (await db.get('households', 'h1'))!, old, true)
    const issues = (await queue.list()).filter((i) => i.path === '/v1/households/h1/cards')
    expect(issues).toHaveLength(2)
    for (const { body } of issues) {
      const { code, client_issued_at } = body as { code: string; client_issued_at: string }
      expect(code).toMatch(/^[0-9A-HJKMNP-TV-Z]{8}$/)
      expect(new Date(client_issued_at).toISOString()).toBe(client_issued_at)
    }
    expect((issues[1].body as { code: string }).code).not.toBe('ABCD1234')
  })

  it('cancel without a substitute leaves the household with no card', async () => {
    const queue = make()
    const household = await createHousehold(db, queue, who, { id: 'h1', member_ids: ['p1'] })
    await issueCard(db, queue, who, household, 'ABCD1234')
    expect(await cancelCard(db, queue, who, (await db.get('households', 'h1'))!, 'ABCD1234', false)).toBeNull()
    expect((await db.get('households', 'h1'))?.card_code).toBeUndefined()
  })

  it('a synced cancel and card clear `pending` in the cache', async () => {
    fetchMock.mockResolvedValue(json(201))
    const queue = make()
    const household = await createHousehold(db, queue, who, { id: 'h1', member_ids: ['p1'] })
    await issueCard(db, queue, who, household, 'ABCD1234')
    await queue.drain()
    expect(await db.count('queue')).toBe(0)
    expect(await db.get('households', 'h1')).toMatchObject({ pending: false })
    expect(await db.get('cards', 'ABCD1234')).toMatchObject({ pending: false })
  })
})
