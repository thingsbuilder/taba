import { expect, it } from 'vitest'
import type { QueueItem } from './db'
import { pendingViews } from './pending'

const item = (over: Partial<QueueItem>): QueueItem => ({
  id: 'i', method: 'POST', path: '/v1/shelters', body: {}, volunteer_id: 'ana', created_at: 1, attempts: 0, next_attempt_at: 0, state: 'pending', ...over,
})

it('lists only items that wait for a person, with a label and a translated error key', () => {
  const views = pendingViews([
    item({ id: 'a' }),
    item({ id: 'b', state: 'held', error: 'shelter_nearby', shelter_id: 's1' }),
    item({ id: 'c', method: 'PATCH', path: '/v1/shelters/s1', state: 'needs_action', error: 'weird', shelter_id: 's1' }),
    item({ id: 'd', method: 'PATCH', path: '/v1/me', state: 'needs_action' }),
  ])
  expect(views).toEqual([
    { id: 'b', labelKey: 'pending.create_shelter', state: 'held', errorKey: 'error.shelter_nearby', retryable: true, shelterId: 's1' },
    { id: 'c', labelKey: 'pending.update_shelter', state: 'needs_action', errorKey: 'error.generic', retryable: true, shelterId: 's1' },
    { id: 'd', labelKey: 'pending.update_profile', state: 'needs_action', errorKey: undefined, retryable: true, shelterId: undefined },
  ])
})

it('does not offer to send again what the daily search limit refused', () => {
  const views = pendingViews([item({ id: 'a', method: 'POST', path: '/v1/searches', state: 'needs_action', error: 'search_limit' })])
  expect(views.map((v) => [v.id, v.retryable])).toEqual([['a', false]])
})

it('can be limited to one shelter', () => {
  const views = pendingViews([item({ id: 'b', state: 'held', shelter_id: 's1' }), item({ id: 'x', state: 'held', shelter_id: 's2' })], 's2')
  expect(views.map((v) => v.id)).toEqual(['x'])
})
