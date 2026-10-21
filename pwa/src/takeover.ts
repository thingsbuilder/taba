import type { Db, QueueItem } from './db'
import { type LocalSession, roleIn } from './sessions'

// "Assumir pendências de <nome>" (spec 9.2, M1).

export interface TakeOverOffer {
  fromVolunteerId: string
  /** Shelters where the active operator is admin and the other volunteer left items. */
  shelterIds: string[]
  count: number
}

/**
 * Items signed by a volunteer who has no valid session on this phone cannot go out (they answer `401`).
 * An admin of the item's shelter, with an active session, may take them over.
 */
export function takeOverOffers(items: QueueItem[], active: LocalSession | null, validSessions: LocalSession[]): TakeOverOffer[] {
  if (!active) return []
  const withSession = new Set(validSessions.map((s) => s.volunteer_id))
  const offers = new Map<string, TakeOverOffer>()
  for (const item of items) {
    const from = item.volunteer_id
    if (!from || from === active.volunteer_id || withSession.has(from)) continue
    if (!item.shelter_id || roleIn(active, item.shelter_id) !== 'admin') continue
    const offer = offers.get(from) ?? { fromVolunteerId: from, shelterIds: [], count: 0 }
    if (!offer.shelterIds.includes(item.shelter_id)) offer.shelterIds.push(item.shelter_id)
    offer.count++
    offers.set(from, offer)
  }
  return [...offers.values()]
}

const NAMES_KEY = 'volunteer_names'

/** Remembers who is who while their session exists: the session is erased on `401`, the name is still needed. */
export async function rememberNames(db: Db, sessions: LocalSession[]): Promise<void> {
  const known = ((await db.get('config', NAMES_KEY)) as Record<string, string> | undefined) ?? {}
  const next = { ...known, ...Object.fromEntries(sessions.map((s) => [s.volunteer_id, s.profile.name])) }
  if (JSON.stringify(next) !== JSON.stringify(known)) await db.put('config', next, NAMES_KEY)
}

export async function nameOf(db: Db, volunteerId: string): Promise<string | null> {
  return (((await db.get('config', NAMES_KEY)) as Record<string, string> | undefined) ?? {})[volunteerId] ?? null
}
