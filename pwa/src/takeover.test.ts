import { expect, it } from 'vitest'
import type { QueueItem } from './db'
import { phase3Label, queueErrorKey } from './queue-errors'
import type { LocalSession } from './sessions'
import { nameOf, rememberNames, takeOverOffers } from './takeover'
import { freshDb } from './testing'

const session = (id: string, admin: string[] = []): LocalSession => ({
  volunteer_id: id, secret: 's', expires_at: 1, ttl_ms: 1, global_roles: [], failed_pins: 0,
  profile: { name: id.toUpperCase(), phone: null, show_phone: false, language: 'pt-BR' },
  memberships: admin.map((shelter_id) => ({ shelter_id, name: shelter_id, role: 'admin' as const, confirmation: 'confirmed' as const })),
})
const item = (over: Partial<QueueItem>): QueueItem => ({
  id: 'i', method: 'POST', path: '/v1/shelters/s1/checkins', body: {}, volunteer_id: 'ana', shelter_id: 's1', created_at: 1, attempts: 0, next_attempt_at: 0, state: 'pending', ...over,
})

it('offers the items of a volunteer without a session, only in shelters where the active operator is admin', () => {
  const bia = session('bia', ['s1'])
  const offers = takeOverOffers(
    [item({ id: 'a' }), item({ id: 'b', shelter_id: 's2' }), item({ id: 'c', volunteer_id: 'cris' }), item({ id: 'd', volunteer_id: 'bia' }), item({ id: 'e', volunteer_id: null })],
    bia,
    [bia, session('cris')],
  )
  expect(offers).toEqual([{ fromVolunteerId: 'ana', shelterIds: ['s1'], count: 1 }])
})

it('offers nothing to a plain volunteer or when nobody is active', () => {
  expect(takeOverOffers([item({})], session('bia'), [])).toEqual([])
  expect(takeOverOffers([item({})], null, [])).toEqual([])
})

it('remembers the names while the session exists', async () => {
  const db = await freshDb()
  await rememberNames(db, [session('ana')])
  await rememberNames(db, [session('bia')]) // ana's session is gone by now
  expect(await nameOf(db, 'ana')).toBe('ANA')
  expect(await nameOf(db, 'zed')).toBeNull()
})

it('names phase 3 items and their conflicts', () => {
  expect(phase3Label({ method: 'POST', path: '/v1/shelters/s1/checkins' })).toBe('queue.label_checkin')
  expect(phase3Label({ method: 'PATCH', path: '/v1/people/p1' })).toBe('queue.label_person_edit')
  expect(phase3Label({ method: 'PUT', path: '/v1/people/p1/biometrics' })).toBe('queue.label_biometrics')
  expect(phase3Label({ method: 'POST', path: '/v1/cards/ABCD1234/cancel' })).toBe('queue.label_card_cancel')
  expect(phase3Label({ method: 'PATCH', path: '/v1/shelters/s1' })).toBeNull()
  expect(queueErrorKey('person_in_other_household')).toBe('queue.error_person_in_other_household')
  expect(queueErrorKey('household_has_card')).toBe('queue.error_household_has_card')
  expect(queueErrorKey('shelter_nearby')).toBe('error.shelter_nearby')
})
