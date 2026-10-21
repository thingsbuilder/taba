import type { Db } from './db'
import type { Queue } from './queue'

// Address lookup from the phone (spec 6.1.6): at most 1 request per second (Nominatim policy),
// results cached in IndexedDB, and a pending entry in `addresses` while there is no signal.

export interface Address {
  text: string
  city: string
  state: string
}

const MIN_INTERVAL_MS = 1000

interface NominatimReply {
  display_name?: string
  address?: Record<string, string | undefined>
}

/** City from `city`/`town`/`village`/`municipality`; UF from `ISO3166-2-lvl4` ("BR-RS" gives "RS"). */
export function parseAddress(reply: NominatimReply): Address | null {
  if (!reply.display_name) return null
  const a = reply.address ?? {}
  const iso = a['ISO3166-2-lvl4']
  return {
    text: reply.display_name,
    city: a.city ?? a.town ?? a.village ?? a.municipality ?? '',
    state: iso?.includes('-') ? iso.split('-')[1]! : '',
  }
}

const cacheKey = (lat: number, lon: number) => `nominatim:${lat.toFixed(4)},${lon.toFixed(4)}`

export interface NominatimDeps {
  db: Db
  queue: Queue
  fetch: typeof fetch
  now?: () => number
  sleep?: (ms: number) => Promise<void>
}

export function createNominatim({ db, queue, fetch: fetchFn, now = Date.now, sleep = (ms) => new Promise((r) => setTimeout(r, ms)) }: NominatimDeps) {
  let last = -Infinity
  let chain: Promise<unknown> = Promise.resolve()

  /** Address for the coordinates; null when the lookup failed (no signal, server error). */
  async function reverse(lat: number, lon: number): Promise<Address | null> {
    const cached = (await db.get('config', cacheKey(lat, lon))) as Address | undefined
    if (cached) return cached
    const run = async (): Promise<Address | null> => {
      const wait = last + MIN_INTERVAL_MS - now()
      if (wait > 0) await sleep(wait)
      last = now()
      try {
        const res = await fetchFn(`https://nominatim.openstreetmap.org/reverse?format=jsonv2&lat=${lat}&lon=${lon}&accept-language=pt-BR`)
        if (!res.ok) return null
        const address = parseAddress((await res.json()) as NominatimReply)
        if (address) await db.put('config', address, cacheKey(lat, lon))
        return address
      } catch {
        return null
      }
    }
    const result = chain.then(run, run)
    chain = result
    return result
  }

  const patch = (shelterId: string, volunteerId: string | null, address: Address) =>
    queue.enqueue({ method: 'PATCH', path: `/v1/shelters/${shelterId}`, body: { address }, volunteer_id: volunteerId, shelter_id: shelterId })

  /** After a shelter was created without an address: look it up now or keep the coordinates for later. */
  async function resolveFor(shelterId: string, lat: number, lon: number, volunteerId: string | null): Promise<void> {
    const address = await reverse(lat, lon)
    if (address) await patch(shelterId, volunteerId, address)
    else await db.put('addresses', { shelter_id: shelterId, lat, lon, volunteer_id: volunteerId })
  }

  /** Signal is back (or the app opened): try the waiting coordinates again. */
  async function retryPending(): Promise<void> {
    for (const entry of await db.getAll('addresses')) {
      const address = await reverse(entry.lat, entry.lon)
      if (!address) continue
      await patch(entry.shelter_id, entry.volunteer_id, address)
      await db.delete('addresses', entry.shelter_id)
    }
  }

  return { reverse, resolveFor, retryPending }
}
