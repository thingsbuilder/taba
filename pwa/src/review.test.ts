import { expect, it } from 'vitest'
import { reasonText, reviewErrorKey } from './review'

it('names the other shelter only when the API sent it', () => {
  const base = { kind: 'duplicate_in_period' as const, previous_delivered_at: '2026-10-10T12:00:00Z' }
  expect(reasonText(base).key).toBe('review.reason_duplicate')
  expect(reasonText(base).params).not.toHaveProperty('shelter')
  expect(reasonText({ ...base, previous_shelter_name: 'Escola Central' })).toMatchObject({ key: 'review.reason_duplicate_at', params: { shelter: 'Escola Central' } })
})
it('has a text for the missing selfie', () => expect(reasonText({ kind: 'liveness_unavailable' }).key).toBe('review.reason_offline'))
it('409 is "already decided"', () => {
  expect(reviewErrorKey(409)).toBe('review.error_decided')
  expect(reviewErrorKey(500)).toBeNull()
})
