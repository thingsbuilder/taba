import type { PersonSummary } from './api-phase3'
import type { Db, PersonRecord, QueueItem } from './db'

// What the people list and the person screen show (design/README.md: a person without a name is
// "Pulseira 0217", never "Desconhecido").

export type DisplayName = { kind: 'name'; text: string } | { kind: 'wristband'; text: string } | { kind: 'none' }

/** The social name first: it is the name the person uses. */
export function displayName(p: { name?: string; social_name?: string; wristband?: string }): DisplayName {
  const name = p.social_name?.trim() || p.name?.trim()
  if (name) return { kind: 'name', text: name }
  const wristband = p.wristband?.trim()
  return wristband ? { kind: 'wristband', text: wristband } : { kind: 'none' }
}

/** Two initials for the avatar; `#` for a wristband or nobody. */
export function initials(name: DisplayName): string {
  if (name.kind !== 'name') return '#'
  const words = name.text.split(/\s+/).filter(Boolean)
  return ((words[0]?.[0] ?? '') + (words.length > 1 ? words[words.length - 1]![0]! : '')).toUpperCase()
}

/** "há 2 min" in the interface language (spec 11.1, design: relative times in lists). */
export function relativeTime(iso: string, now: number, language: string): string {
  const diffSeconds = Math.round((Date.parse(iso) - now) / 1000)
  const units: [Intl.RelativeTimeFormatUnit, number][] = [['day', 86400], ['hour', 3600], ['minute', 60]]
  const rtf = new Intl.RelativeTimeFormat(language, { numeric: 'auto' })
  for (const [unit, size] of units) {
    if (Math.abs(diffSeconds) >= size) return rtf.format(Math.round(diffSeconds / size), unit)
  }
  return rtf.format(0, 'second')
}

const fold = (s: string) => s.normalize('NFD').replace(/\p{M}/gu, '').toLowerCase()

/** Search by name, social name or wristband, ignoring case and accents. */
export function matchesQuery(p: { name?: string; social_name?: string; wristband?: string }, query: string): boolean {
  const q = fold(query.trim())
  if (q === '') return true
  return [p.name, p.social_name, p.wristband].some((v) => v !== undefined && fold(v).includes(q))
}

export interface PendingPerson {
  queue_id: string
  name?: string
  social_name?: string
  wristband?: string
  is_child: boolean
  created_at: number
  state: QueueItem['state']
}

/** People registered on this phone whose check-in still waits in the queue of the shelter. */
export function pendingPeople(items: QueueItem[], shelterId: string): PendingPerson[] {
  return items
    .filter((i) => i.method === 'POST' && i.path === `/v1/shelters/${shelterId}/checkins`)
    .map((i) => {
      const data = ((i.body as { data?: PendingPerson } | null)?.data ?? {}) as Partial<PendingPerson>
      return {
        queue_id: i.id,
        name: data.name,
        social_name: data.social_name,
        wristband: data.wristband,
        is_child: data.is_child === true,
        created_at: i.created_at,
        state: i.state,
      }
    })
}

const toRecord = (p: PersonSummary, shelterId: string): PersonRecord => ({
  id: p.id,
  current_shelter_id: shelterId,
  name: p.name,
  wristband: p.wristband,
  level: p.level,
  is_child: p.is_child,
  child_status: p.child_status,
  household_id: p.household_id,
  updated_at: p.updated_at,
})

/**
 * The phone's copy of the shelter's list, for reading offline: replaces what was there. A page cut by
 * the server (`hasMore`) is not the whole list, so it only adds to the copy: the rest is not dropped.
 */
export async function cachePeople(db: Db, shelterId: string, people: PersonSummary[], hasMore = false): Promise<void> {
  const tx = db.transaction('people', 'readwrite')
  const store = tx.objectStore('people')
  // Records made on this phone and not confirmed yet are not the server's to replace.
  if (!hasMore) for (const old of await store.index('by_shelter').getAll(shelterId)) if (!old.pending) await store.delete(old.id)
  for (const p of people) await store.put(toRecord(p, shelterId))
  await tx.done
}

export async function cachedPeople(db: Db, shelterId: string): Promise<PersonSummary[]> {
  const rows = await db.getAllFromIndex('people', 'by_shelter', shelterId)
  return rows
    // Pending ones are shown from the queue (`pendingPeople`).
    .filter((r) => r.level && !r.pending)
    .map((r) => ({
      id: r.id,
      level: r.level!,
      name: r.name,
      wristband: r.wristband,
      is_child: r.is_child === true,
      child_status: r.child_status,
      household_id: r.household_id,
      has_thumbnail: false,
      updated_at: r.updated_at ?? '',
    }))
}
