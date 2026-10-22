import { describe, expect, it } from 'vitest'
import { itemsErrorKey, itemsFromRows, newRow, rowsFromItems, validateRows } from './items'

describe('items rows', () => {
  it('round-trips the map, with the period at 7 when absent', () => {
    const rows = rowsFromItems({ Basket: { scope: 'per_household', once_per_event: false, high_value: false }, Mattress: { scope: 'per_person', once_per_event: true, high_value: true } })
    expect(rows[0]).toMatchObject({ name: 'Basket', periodDays: '7' })
    expect(itemsFromRows(rows)).toEqual({
      Basket: { scope: 'per_household', period_days: 7, once_per_event: false, high_value: false },
      Mattress: { scope: 'per_person', once_per_event: true, high_value: true },
    })
  })
  it('trims names and drops the period of a once-per-event item', () => {
    expect(itemsFromRows([{ ...newRow(), name: ' Kit ', oncePerEvent: true, periodDays: '' }])).toEqual({ Kit: { scope: 'per_person', once_per_event: true, high_value: false } })
  })
})

describe('validateRows', () => {
  const row = (name: string, extra = {}) => ({ ...newRow(), name, ...extra })
  it('accepts a valid list and the empty list', () => {
    expect(validateRows([row('A'), row('B', { periodDays: '30' })])).toBeNull()
    expect(validateRows([])).toBeNull()
  })
  it('refuses an empty name', () => expect(validateRows([row('  ')])).toBe('items.error_name_empty'))
  it('refuses names that repeat ignoring case and spaces', () => expect(validateRows([row('Kit'), row(' kit ')])).toBe('items.error_name_duplicate'))
  it('refuses a period below 1, empty or not a whole number', () => {
    for (const p of ['0', '-3', '', '1.5', 'x']) expect(validateRows([row('A', { periodDays: p })]), p).toBe('items.error_period')
  })
  it('ignores the period of a once-per-event item', () => expect(validateRows([row('A', { oncePerEvent: true, periodDays: '0' })])).toBeNull())
})

it('gives 403 and 409 their own texts', () => {
  expect(itemsErrorKey(403)).toBe('items.error_forbidden')
  expect(itemsErrorKey(409)).toBe('items.error_closed')
  expect(itemsErrorKey(500)).toBeNull()
})
