import type { Db } from './db'

// Local diagnostic log: the last events of this phone, kept in `config.diagnostics_log`, for the
// volunteer to copy and send when something fails that the server log cannot see. It records what
// happened (route, status, counts), never who it happened to: see `sanitize`.

export const LOG_LIMIT = 200
const KEY = 'diagnostics_log'
/** One value is cut to this many characters. */
const MAX_VALUE = 120

export type Detail = Record<string, string | number | boolean | null | undefined>
export interface LogEvent {
  t: number
  kind: string
  detail: Detail
}

declare const __APP_VERSION__: string
export const APP_VERSION = typeof __APP_VERSION__ === 'string' ? __APP_VERSION__ : 'dev'

let store: Db | null = null
// One write at a time: two events in the same tick must not overwrite each other.
let chain: Promise<void> = Promise.resolve()

export function setDiagnosticsDb(db: Db | null): void {
  store = db
}

/** Keys that hold personal data or secrets: never stored, whatever the value. */
const DENIED_KEY = /name|phone|address|photo|thumbnail|image|base64|body|secret|token|authorization|session|wristband|code|pin|note/i
const ID_KEY = /(^|_)id$/

function cleanString(value: string): string {
  return value
    .replace(/Bearer\s+\S+/gi, 'Bearer [x]')
    .replace(/data:[^\s,]*,\S*/gi, '[data]')
    .replace(/[A-Za-z0-9+/_=-]{40,}/g, '[blob]')
    .replace(/\+?\d[\d\s().-]{6,}\d/g, '[num]')
    .slice(0, MAX_VALUE)
}

/** Flat values only; ids cut to their first 6 characters; personal keys and anything that looks like a secret dropped. */
export function sanitize(detail: Detail = {}): Detail {
  const out: Detail = {}
  for (const [key, value] of Object.entries(detail)) {
    if (value === undefined || value === null || DENIED_KEY.test(key)) continue
    if (typeof value === 'string') out[key] = ID_KEY.test(key) ? value.slice(0, 6) : cleanString(value)
    else if (typeof value === 'number' || typeof value === 'boolean') out[key] = value
  }
  return out
}

/** `/v1/people/ab12.../biometrics` as `/v1/people/ab12cd/biometrics`: ids cut, codes of invites, cards and captures hidden. */
export function routeOf(path: string): string {
  const parts = path.split('?')[0]!.split('/')
  return parts
    .map((part, i) => {
      if (/^(invites|cards|captures|endorsements|search-links)$/.test(parts[i - 1] ?? '')) return ':code'
      return /\d/.test(part) && part.length >= 8 ? part.slice(0, 6) : part
    })
    .join('/')
}

/** Never throws and never waits: the app does not depend on its own log. */
export function logEvent(kind: string, detail?: Detail): void {
  const db = store
  if (!db) return
  const event: LogEvent = { t: Date.now(), kind, detail: sanitize(detail) }
  chain = chain
    .then(async () => {
      const log = ((await db.get('config', KEY)) as LogEvent[] | undefined) ?? []
      await db.put('config', [...log, event].slice(-LOG_LIMIT), KEY)
    })
    .catch(() => undefined)
}

/** Oldest first. Empty when the log cannot be read. */
export async function readLog(): Promise<LogEvent[]> {
  try {
    await chain
    return store ? (((await store.get('config', KEY)) as LogEvent[] | undefined) ?? []) : []
  } catch {
    return []
  }
}

export async function clearLog(): Promise<void> {
  try {
    await chain
    await store?.delete('config', KEY)
  } catch {
    // nothing to clear
  }
}

/** OS and browser with their versions, not the whole user agent. */
export function uaSummary(ua: string): string {
  const os = /(iPhone OS|CPU OS|Android|Mac OS X|Windows NT)[ _]?([\d_.]*)/.exec(ua)
  const browser = /(Version|CriOS|Chrome|FxiOS|Firefox|EdgA?)\/([\d.]+)/.exec(ua)
  return [os && `${os[1]} ${os[2]!.replace(/_/g, '.')}`.trim(), browser && `${browser[1]} ${browser[2]}`].filter(Boolean).join(' / ') || 'unknown'
}

/** Text to paste in a message: a header about the app and the phone, then the events, newest first. */
export function formatLog(
  events: LogEvent[],
  env: { version: string; userAgent: string; online: boolean; now: number },
): string {
  const header = [
    `Taba ${env.version}`,
    `Browser: ${uaSummary(env.userAgent)}`,
    `Online: ${env.online ? 'yes' : 'no'}`,
    `Time: ${new Date(env.now).toISOString()}`,
  ]
  const lines = [...events].reverse().map((e) => {
    const detail = Object.entries(e.detail).map(([k, v]) => `${k}=${v}`).join(' ')
    return `${new Date(e.t).toISOString()} ${e.kind}${detail ? ` ${detail}` : ''}`
  })
  return [...header, '', ...lines].join('\n')
}
