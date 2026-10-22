import type { ItemScope, ItemSpec, Items } from './api-phase5'

export const DEFAULT_PERIOD_DAYS = 7

/** One row of the items screen. `periodDays` stays a string so the field can be empty while typing. */
export interface ItemRow {
  name: string
  scope: ItemScope
  periodDays: string
  oncePerEvent: boolean
  highValue: boolean
}

export const newRow = (): ItemRow => ({ name: '', scope: 'per_person', periodDays: String(DEFAULT_PERIOD_DAYS), oncePerEvent: false, highValue: false })

export const rowsFromItems = (items: Items): ItemRow[] =>
  Object.entries(items).map(([name, spec]) => ({
    name,
    scope: spec.scope,
    periodDays: String(spec.period_days ?? DEFAULT_PERIOD_DAYS),
    oncePerEvent: spec.once_per_event,
    highValue: spec.high_value,
  }))

const periodOf = (row: ItemRow): number => Number(row.periodDays)
const validPeriod = (row: ItemRow): boolean => row.oncePerEvent || (row.periodDays.trim() !== '' && Number.isInteger(periodOf(row)) && periodOf(row) >= 1)

/** i18n key of the first problem, or null. Names are unique ignoring case and surrounding spaces. */
export function validateRows(rows: ItemRow[]): string | null {
  const seen = new Set<string>()
  for (const row of rows) {
    const name = row.name.trim().toLowerCase()
    if (name === '') return 'items.error_name_empty'
    if (seen.has(name)) return 'items.error_name_duplicate'
    seen.add(name)
    if (!validPeriod(row)) return 'items.error_period'
  }
  return null
}

export function itemsFromRows(rows: ItemRow[]): Items {
  const items: Items = {}
  for (const row of rows) {
    const spec: ItemSpec = { scope: row.scope, once_per_event: row.oncePerEvent, high_value: row.highValue }
    if (!row.oncePerEvent) spec.period_days = periodOf(row)
    items[row.name.trim()] = spec
  }
  return items
}

/** `403` and `409` get their own texts; the rest follow the shared error codes. */
export function itemsErrorKey(status: number): string | null {
  if (status === 403) return 'items.error_forbidden'
  if (status === 409) return 'items.error_closed'
  return null
}
