import { expect, it } from 'vitest'
import type { AlertView } from './api'
import { alertLink, alertTextKey, countdown } from './alerts'

const alert = (over: Partial<AlertView>): AlertView => ({
  id: 'a', kind: 'shelter_flagged', informational: false, target_kind: 'shelter', target_id: 's1', event_id: null,
  shelter_ids: [], volunteer_ids: [], detail: {}, created_at: '', resolved: false, ...over,
})

it('maps the four phase 2 kinds and falls back for the others', () => {
  for (const k of ['shelter_flagged', 'event_overlap', 'volunteer_removed', 'endorser_flagged']) expect(alertTextKey(k)).toBe(`alert.${k}`)
  expect(alertTextKey('child_separated')).toBe('alert.generic')
})

it('links shelter alerts, and single-shelter ones, to the shelter screen', () => {
  expect(alertLink(alert({}))).toBe('/shelter/s1')
  expect(alertLink(alert({ target_kind: 'volunteer', shelter_ids: ['s9'] }))).toBe('/shelter/s9')
  expect(alertLink(alert({ target_kind: 'volunteer', shelter_ids: ['a', 'b'] }))).toBeNull()
  expect(alertLink(alert({ target_kind: 'volunteer' }))).toBeNull()
})

it('counts down as m:ss and stops at zero', () => {
  const end = '2026-10-21T12:10:00Z'
  const t0 = Date.parse('2026-10-21T12:00:00Z')
  expect(countdown(end, t0)).toBe('10:00')
  expect(countdown(end, t0 + 9 * 60_000 + 55_500)).toBe('0:05')
  expect(countdown(end, t0 + 11 * 60_000)).toBe('0:00')
})
