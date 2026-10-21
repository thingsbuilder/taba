import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { Db } from './db'
import en from './i18n/en.json'
import ptBR from './i18n/pt-BR.json'
import { conflictNames, pendingViews } from './pending'
import { createQueue } from './queue'
import { freshDb } from './testing'

// Conflicts of spec 9.4: the server refuses a household or a card the phone already shows.

const json = (status: number, body: unknown = {}) => new Response(JSON.stringify(body), { status })

describe('household and card conflicts in the queue', () => {
  let db: Db
  const fetchMock = vi.fn<typeof fetch>()
  const make = () => createQueue({ db, fetch: fetchMock, deviceId: async () => 'd', getStatus: async () => ({ unico: 'ok' }), random: () => 0.5 })

  beforeEach(async () => {
    db = await freshDb()
    fetchMock.mockReset()
    await db.put('people', { id: 'p1', name: 'Ana', current_shelter_id: 's1', household_id: 'h-new', pending: true })
    await db.put('people', { id: 'p2', name: 'Bia', current_shelter_id: 's1', household_id: 'h-other' })
    await db.put('households', { id: 'h-other', member_ids: ['p2'], shelter_id: 's1' })
    await db.put('households', { id: 'h-new', member_ids: ['p1'], shelter_id: 's1', pending: true })
  })

  const household = { method: 'POST', path: '/v1/shelters/s1/households', body: { id: 'h-new', member_ids: ['p1'], client_created_at: 'x' }, volunteer_id: 'ana', shelter_id: 's1' }

  it('409 person_in_other_household waits for a person with the household named in the message', async () => {
    fetchMock.mockResolvedValue(json(409, { error: 'person_in_other_household', detail: { person_id: 'p2', household_id: 'h-other' } }))
    const queue = make()
    await queue.enqueue(household)
    await queue.drain()
    const [item] = await queue.list()
    expect(item).toMatchObject({ state: 'needs_action', error: 'person_in_other_household', detail: { person_id: 'p2', household_id: 'h-other' } })

    const names = await conflictNames(db, [item!])
    expect(names).toEqual({ people: { p2: 'Bia' }, households: { 'h-other': 'Bia' } })
    const [view] = pendingViews([item!], undefined, names)
    expect(view).toMatchObject({ errorKey: 'queue.error_person_in_other_household_named', errorParams: { person: 'Bia', household: 'Bia' } })
    for (const dict of [ptBR, en] as Record<string, string>[]) {
      const text = dict[view!.errorKey!]!.replace('{person}', 'Bia').replace('{household}', 'Bia')
      expect(text).toContain('Bia')
      expect(text).not.toContain('{')
    }
  })

  it('without knowing the household it falls back to the general text', async () => {
    const item = { id: 'i', ...household, created_at: 1, attempts: 0, next_attempt_at: 0, state: 'needs_action' as const, error: 'person_in_other_household', detail: { person_id: 'p9', household_id: 'h-unknown' } }
    const [view] = pendingViews([item], undefined, await conflictNames(db, [item]))
    expect(view!.errorKey).toBe('queue.error_person_in_other_household')
    expect(view!.errorParams).toBeUndefined()
  })

  it('discarding the refused household takes it, and its mark on the people, out of the phone', async () => {
    fetchMock.mockResolvedValue(json(409, { error: 'person_in_other_household', detail: { person_id: 'p1', household_id: 'h-other' } }))
    const queue = make()
    const item = await queue.enqueue(household)
    await queue.drain()
    await queue.discard(item.id)
    expect(await db.get('households', 'h-new')).toBeUndefined()
    expect((await db.get('people', 'p1'))?.household_id).toBeUndefined()
    expect((await db.get('people', 'p2'))?.household_id).toBe('h-other') // the one that exists is left alone
    expect(await db.count('queue')).toBe(0)
  })

  it('409 household_has_card names the household and discarding drops the pending card', async () => {
    await db.put('cards', { code: 'ABCD2345', household_id: 'h-other', status: 'active', pending: true })
    await db.put('households', { id: 'h-other', member_ids: ['p2'], shelter_id: 's1', card_code: 'ABCD2345' })
    fetchMock.mockResolvedValue(json(409, { error: 'household_has_card' }))
    const queue = make()
    const item = await queue.enqueue({ method: 'POST', path: '/v1/households/h-other/cards', body: { code: 'ABCD2345', client_issued_at: 'x' }, volunteer_id: 'ana', shelter_id: 's1' })
    await queue.drain()
    const [waiting] = await queue.list()
    expect(waiting).toMatchObject({ state: 'needs_action', error: 'household_has_card' })
    const [view] = pendingViews([waiting!], undefined, await conflictNames(db, [waiting!]))
    expect(view).toMatchObject({ errorKey: 'queue.error_household_has_card_named', errorParams: { household: 'Bia' } })

    await queue.discard(item.id)
    expect(await db.get('cards', 'ABCD2345')).toBeUndefined()
    expect((await db.get('households', 'h-other'))?.card_code).toBeUndefined()
  })

  it('discarding a card that the server already knows (not pending) keeps the local copy', async () => {
    await db.put('cards', { code: 'ZZZZ9999', household_id: 'h-other', status: 'active' })
    const queue = make()
    fetchMock.mockResolvedValue(json(409, { error: 'code_taken' }))
    const item = await queue.enqueue({ method: 'POST', path: '/v1/households/h-other/cards', body: { code: 'ZZZZ9999', client_issued_at: 'x' }, volunteer_id: 'ana', shelter_id: 's1' })
    await queue.drain()
    await queue.discard(item.id)
    expect(await db.get('cards', 'ZZZZ9999')).toBeDefined()
  })
})
