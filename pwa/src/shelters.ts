import type { AddressInput, EventView, Membership, ShelterView } from './api'
import type { Db, QueueItem, ShelterRecord } from './db'
import type { Position } from './geo'
import type { Queue } from './queue'

// The phone's copy of shelters (`shelters` store, spec 6.1.1) and shelter creation (spec 3.2).

const MAX_NAME = 100

export function recordFromView(view: ShelterView, event?: EventView | null): ShelterRecord {
  return {
    id: view.id,
    name: view.name,
    lat: view.lat,
    lon: view.lon,
    address: view.address && { text: view.address.text, city: view.address.city, state: view.address.state },
    capacity: view.capacity,
    confirmation: view.confirmation,
    status: view.status,
    event_name: (event ?? view.event)?.name ?? null,
  }
}

export const saveView = (db: Db, view: ShelterView, event?: EventView | null) => db.put('shelters', recordFromView(view, event))

/** Server answers of queued writes refresh the local copy and clear `pending` (wired in main.tsx). */
export async function applySynced(db: Db, item: QueueItem, body: unknown): Promise<void> {
  const reply = body as { shelter?: ShelterView; event?: EventView } | null
  if (reply?.shelter && item.path.startsWith('/v1/shelters')) await saveView(db, reply.shelter, reply.event)
}

export function createBody(id: string, name: string, p: Position, o: { capacity?: number; address?: AddressInput; now?: Date } = {}) {
  return {
    id,
    name,
    lat: p.lat,
    lon: p.lon,
    accuracy: p.accuracy,
    ...(o.address ? { address: o.address } : {}),
    ...(o.capacity ? { capacity: o.capacity } : {}),
    client_created_at: (o.now ?? new Date()).toISOString(),
  }
}

export type CreateError = 'invalid_shelter' | 'insufficient_accuracy'

/** Spec 3.2: always through the queue; online it goes out at once, offline it waits. */
export async function createShelter(
  db: Db,
  queue: Queue,
  volunteerId: string,
  input: { name: string; position: Position; capacity?: number; address?: AddressInput },
  accuracyOk: (accuracy: number) => boolean,
): Promise<{ ok: true; id: string } | { ok: false; error: CreateError }> {
  const name = input.name.trim()
  if (name === '' || name.length > MAX_NAME) return { ok: false, error: 'invalid_shelter' }
  if (input.capacity !== undefined && (!Number.isInteger(input.capacity) || input.capacity < 1)) return { ok: false, error: 'invalid_shelter' }
  if (!accuracyOk(input.position.accuracy)) return { ok: false, error: 'insufficient_accuracy' }
  const id = crypto.randomUUID()
  await db.put('shelters', {
    id, name, lat: input.position.lat, lon: input.position.lon, address: input.address, capacity: input.capacity ?? null,
    confirmation: 'unconfirmed', status: 'open', pending: true,
  })
  await queue.enqueue({
    method: 'POST', path: '/v1/shelters', body: createBody(id, name, input.position, input), volunteer_id: volunteerId, shelter_id: id,
  })
  return { ok: true, id }
}

export interface Actions {
  members: boolean
  invite: boolean
  requestEndorsement: boolean
  editCapacity: boolean
  close: boolean
  confirm: boolean
  revoke: boolean
  renameEvent: boolean
  editContacts: boolean
}

/** Which buttons the shelter screen shows (matrix 2.3); the server checks again. */
export function actionsFor(role: Membership['role'] | null, globalRoles: string[], view: Pick<ShelterView, 'confirmation' | 'status'>): Actions {
  const admin = role === 'admin'
  const dc = globalRoles.includes('civil_defense')
  const validator = globalRoles.includes('validator') || dc
  const open = view.status !== 'closed'
  return {
    members: role !== null,
    invite: admin && open,
    requestEndorsement: admin && open && view.confirmation === 'unconfirmed',
    editCapacity: admin || dc,
    close: (admin || dc) && open,
    confirm: validator && view.confirmation === 'unconfirmed',
    revoke: validator && view.confirmation === 'confirmed',
    renameEvent: admin || dc,
    editContacts: validator,
  }
}

export interface Conflict {
  name: string
  firstName: string
  phone?: string
  distanceM: number
}

/** The `shelter_nearby` details kept on a held queue item (spec 3.2 step 8). */
export function conflictOf(detail: unknown): Conflict | null {
  const d = detail as { name?: string; distance_m?: number; contact?: { first_name?: string; phone?: string } } | null | undefined
  if (!d?.name || !d.contact?.first_name) return null
  return { name: d.name, firstName: d.contact.first_name, phone: d.contact.phone, distanceM: Math.round(d.distance_m ?? 0) }
}
