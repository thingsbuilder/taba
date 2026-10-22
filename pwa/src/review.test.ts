import { describe, expect, it } from 'vitest'
import { setLanguage } from './i18n'
import { reasonText, reviewErrorKey, reviewMeta } from './review'

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

describe('the line under a delivery of the review list', () => {
  it('names who delivered, when, the card and who approved, skipping what is missing', () => {
    const at = '2026-10-21T13:30:00Z'
    const full = reviewMeta({ delivered_by_name: 'Rui', delivered_at: at, card_code: 'ABCD2345', approved_by_name: 'Ana' }, 'approved by')
    expect(full).toContain('Rui')
    expect(full).toContain('ABCD2345')
    expect(full.endsWith('approved by Ana')).toBe(true)
    expect(reviewMeta({ delivered_at: at }, 'x').split(' · ')).toHaveLength(1)
  })

  it('writes the date in the language of the app', () => {
    const at = '2026-10-21T13:30:00Z'
    setLanguage('en')
    const en = reviewMeta({ delivered_at: at }, '')
    setLanguage('pt-BR')
    const pt = reviewMeta({ delivered_at: at }, '')
    expect(en).not.toBe(pt)
  })
})
