import type { AlertView } from './api'
import { getLanguage } from './i18n'

// Civil Defense panel helpers (spec 3.10): who sees it, number formats, the file download and the anomalies.

/** The panel and the "Shelters to confirm" list are for a validator or the Civil Defense. */
export const canSeeDashboard = (globalRoles: string[]): boolean => globalRoles.includes('validator') || globalRoles.includes('civil_defense')

export const ANOMALY_KINDS = ['anomaly_volume', 'anomaly_distance', 'out_of_area']

/** Open informational anomaly alerts of the event, as `GET /v1/alerts` delivers them. */
export const anomaliesOf = (alerts: AlertView[], eventId: string): AlertView[] =>
  alerts.filter((a) => ANOMALY_KINDS.includes(a.kind) && !a.resolved && (a.event_id === null || a.event_id === eventId))

export const formatNumber = (n: number): string => new Intl.NumberFormat(getLanguage()).format(n)

/** `ratio` from 0 to 1; `null` is "no data". */
export const formatRatio = (ratio: number | null): string =>
  ratio === null ? '–' : new Intl.NumberFormat(getLanguage(), { style: 'percent', maximumFractionDigits: 1 }).format(ratio)

export const formatDecimal = (n: number | null): string =>
  n === null ? '–' : new Intl.NumberFormat(getLanguage(), { maximumFractionDigits: 2 }).format(n)

/** The "updated at" time of an answer, in the language of the app. */
export const formatGeneratedAt = (iso: string): string => new Date(iso).toLocaleString(getLanguage())

/** `403` has its own text; the rest follow the shared error codes. */
export const dashboardErrorKey = (status: number): string | null => (status === 403 ? 'dashboard.error_forbidden' : null)

/** Saves a file from a response: the CSV endpoints answer files, not JSON. */
export async function saveCsv(res: Response, filename: string): Promise<boolean> {
  if (!res.ok) return false
  const url = URL.createObjectURL(await res.blob())
  const a = document.createElement('a')
  a.href = url
  a.download = filename
  document.body.appendChild(a)
  a.click()
  a.remove()
  URL.revokeObjectURL(url)
  return true
}
