import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { ShelterView } from './api'
import type { Db } from './db'
import { accuracyOk } from './geo'
import { createQueue } from './queue'
import { actionsFor, applySynced, conflictOf, createBody, createShelter } from './shelters'
import { freshDb } from './testing'

const position = { lat: -30.03, lon: -51.2, accuracy: 20 }
const view: ShelterView = {
  id: 's1', name: 'Ginásio', confirmation: 'confirmed', capacity: 200, occupancy: 0, vacancies: 200,
  lat: 1, lon: 2, address: { text: 'Rua A', city: 'Porto Alegre', state: 'RS', pending: false }, status: 'open',
  event: { id: 'e1', name: 'Porto Alegre/RS – 10/2026', auto_named: true, reference_contacts: {} },
}

let db: Db
beforeEach(async () => {
  db = await freshDb()
})

describe('createShelter', () => {
  const fetchMock = vi.fn<typeof fetch>()
  const make = () => createQueue({ db, fetch: fetchMock, deviceId: async () => 'd', getStatus: async () => ({ unico: 'ok' }), secretFor: async () => 's' })

  beforeEach(() => fetchMock.mockReset().mockResolvedValue(new Response('{}', { status: 503 })))

  it('saves a pending local copy and queues POST /v1/shelters with the shelter id', async () => {
    const queue = make()
    const result = await createShelter(db, queue, 'ana', { name: ' Ginásio ', position, capacity: 150 }, accuracyOk)
    if (!result.ok) throw new Error('expected ok')
    expect(await db.get('shelters', result.id)).toMatchObject({ id: result.id, name: 'Ginásio', capacity: 150, pending: true, confirmation: 'unconfirmed' })
    const [item] = await queue.list()
    expect(item).toMatchObject({ method: 'POST', path: '/v1/shelters', shelter_id: result.id, volunteer_id: 'ana' })
    expect(item!.body).toMatchObject({ id: result.id, name: 'Ginásio', lat: -30.03, lon: -51.2, accuracy: 20, capacity: 150 })
    expect(Date.parse((item!.body as { client_created_at: string }).client_created_at)).not.toBeNaN()
  })

  it('refuses a weak fix, a blank name and a bad capacity before saving anything', async () => {
    const queue = make()
    expect(await createShelter(db, queue, 'ana', { name: 'A', position: { ...position, accuracy: 101 } }, accuracyOk)).toEqual({ ok: false, error: 'insufficient_accuracy' })
    expect(await createShelter(db, queue, 'ana', { name: '  ', position }, accuracyOk)).toEqual({ ok: false, error: 'invalid_shelter' })
    expect(await createShelter(db, queue, 'ana', { name: 'A', position, capacity: 0 }, accuracyOk)).toEqual({ ok: false, error: 'invalid_shelter' })
    expect(await createShelter(db, queue, 'ana', { name: 'A', position, capacity: 1.5 }, accuracyOk)).toEqual({ ok: false, error: 'invalid_shelter' })
    expect(await db.count('shelters')).toBe(0)
    expect(await queue.count()).toBe(0)
  })

  it('leaves out the address and capacity when there are none', () => {
    const body = createBody('id', 'A', position, { now: new Date(0) })
    expect(body).toEqual({ id: 'id', name: 'A', lat: -30.03, lon: -51.2, accuracy: 20, client_created_at: '1970-01-01T00:00:00.000Z' })
  })
})

describe('applySynced', () => {
  const item = (path: string) => ({ path }) as never
  it('replaces the pending copy with what the server answered', async () => {
    await db.put('shelters', { id: 's1', name: 'Ginásio', pending: true })
    await applySynced(db, item('/v1/shelters'), { shelter: view, event: view.event })
    expect(await db.get('shelters', 's1')).toEqual({
      id: 's1', name: 'Ginásio', lat: 1, lon: 2, address: { text: 'Rua A', city: 'Porto Alegre', state: 'RS' },
      capacity: 200, confirmation: 'confirmed', status: 'open', event_name: 'Porto Alegre/RS – 10/2026',
    })
  })
  it('ignores answers without a shelter and non-shelter paths', async () => {
    await applySynced(db, item('/v1/shelters/s1/invites'), { code: 'x' })
    await applySynced(db, item('/v1/me'), { shelter: view })
    expect(await db.count('shelters')).toBe(0)
  })
})

describe('actionsFor (matrix 2.3)', () => {
  const nc = { confirmation: 'unconfirmed' as const, status: 'open' as const }
  const c = { confirmation: 'confirmed' as const, status: 'open' as const }
  const on = (a: ReturnType<typeof actionsFor>) => Object.entries(a).filter(([, v]) => v).map(([k]) => k).sort()

  it('a volunteer only sees the member list', () => {
    expect(on(actionsFor('volunteer', [], nc))).toEqual(['members'])
  })
  it('an admin of an unconfirmed shelter can invite, ask for an endorsement, edit capacity, close and rename the event', () => {
    expect(on(actionsFor('admin', [], nc))).toEqual(['close', 'editCapacity', 'invite', 'members', 'renameEvent', 'requestEndorsement'])
  })
  it('a confirmed shelter no longer asks for an endorsement', () => {
    expect(actionsFor('admin', [], c).requestEndorsement).toBe(false)
  })
  it('a validator confirms or revokes and edits the contacts, nothing else', () => {
    expect(on(actionsFor(null, ['validator'], nc))).toEqual(['confirm', 'editContacts'])
    expect(on(actionsFor(null, ['validator'], c))).toEqual(['editContacts', 'revoke'])
  })
  it('civil defense also edits capacity, closes and renames the event', () => {
    expect(on(actionsFor(null, ['civil_defense'], nc))).toEqual(['close', 'confirm', 'editCapacity', 'editContacts', 'renameEvent'])
  })
  it('a closed shelter takes no invites and cannot be closed again', () => {
    const closed = actionsFor('admin', [], { confirmation: 'unconfirmed', status: 'closed' })
    expect([closed.invite, closed.close, closed.requestEndorsement]).toEqual([false, false, false])
  })
  it('a non-member without a global role has nothing', () => {
    expect(on(actionsFor(null, [], nc))).toEqual([])
  })
})

describe('conflictOf', () => {
  it('reads the 409 details', () => {
    expect(conflictOf({ shelter_id: 'x', name: 'Ginásio', distance_m: 79.6, contact: { first_name: 'Rui', phone: '55' } }))
      .toEqual({ name: 'Ginásio', firstName: 'Rui', phone: '55', distanceM: 80 })
    expect(conflictOf({ name: 'G', contact: { first_name: 'Rui' } })).toEqual({ name: 'G', firstName: 'Rui', phone: undefined, distanceM: 0 })
  })
  it('returns null when the details are missing', () => {
    expect(conflictOf(undefined)).toBeNull()
    expect(conflictOf({})).toBeNull()
  })
})
