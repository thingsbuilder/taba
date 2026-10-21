import { expect, it } from 'vitest'
import type { AlertView } from './api'
import { alertLink, alertPhones, alertTextKey, countdown } from './alerts'
import en from './i18n/en.json'
import ptBR from './i18n/pt-BR.json'

const alert = (over: Partial<AlertView>): AlertView => ({
  id: 'a', kind: 'shelter_flagged', informational: false, target_kind: 'shelter', target_id: 's1', event_id: null,
  shelter_ids: [], volunteer_ids: [], detail: {}, created_at: '', resolved: false, ...over,
})

it('maps every kind of phases 2 and 3 to its own text and falls back for the others', () => {
  const kinds = ['shelter_flagged', 'event_overlap', 'volunteer_removed', 'endorser_flagged', 'child_separated', 'revalidation_failed', 'exceptional_exit', 'household_conflict', 'unico_limit']
  for (const k of kinds) {
    expect(alertTextKey(k)).toBe(`alert.${k}`)
    expect(ptBR, k).toHaveProperty([`alert.${k}`])
    expect(en, k).toHaveProperty([`alert.${k}`])
  }
  expect(alertTextKey('anomaly_volume')).toBe('alert.generic')
})

it('an alert about a person opens the person', () => {
  expect(alertLink(alert({ kind: 'child_separated', target_kind: 'person', target_id: 'p9', shelter_ids: ['s1'] }))).toBe('/person/p9')
})

it('shows the reference phones of a separated child, child protection first', () => {
  const child = alert({ kind: 'child_separated', target_kind: 'person', detail: { reference_contacts: { civil_defense_phone: '199', child_protection_phone: '100' } } })
  const phones = alertPhones(child)
  expect(phones).toEqual([
    { textKey: 'alert.phone_child_protection', phone: '100' },
    { textKey: 'alert.phone_civil_defense', phone: '199' },
  ])
  // The texts exist in both languages and carry the number.
  for (const dict of [ptBR, en] as Record<string, string>[]) {
    for (const p of phones) expect(dict[p.textKey]!.replace('{phone}', p.phone)).toContain(p.phone)
  }
  expect(alertPhones(alert({ kind: 'child_separated', detail: { reference_contacts: { civil_defense_phone: '199' } } }))).toEqual([{ textKey: 'alert.phone_civil_defense', phone: '199' }])
  expect(alertPhones(alert({ kind: 'child_separated', detail: {} }))).toEqual([])
  expect(alertPhones(alert({ kind: 'shelter_flagged', detail: { reference_contacts: { civil_defense_phone: '199' } } }))).toEqual([])
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
