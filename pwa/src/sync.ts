import type { Db, PersonRecord, QueueItem } from './db'

// What a synced queue item changes on the phone (spec 9.2: "atualiza caches") for people,
// households and cards, and the swap of a person's id when the server derived another one (spec 4.12).

/** `value` with every string equal to `from` replaced by `to`, at any depth. */
export function replaceId<T>(value: T, from: string, to: string): T {
  if (value === from) return to as T
  if (Array.isArray(value)) return value.map((v) => replaceId(v, from, to)) as T
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, replaceId(v, from, to)])) as T
  }
  return value
}

const replaceInPath = (path: string, from: string, to: string) => path.split('/').map((s) => (s === from ? to : s)).join('/')

/** Queued items that still mention the old id (a household, an exit, a guardian) now mention the new one. */
export async function remapQueuedIds(db: Db, from: string, to: string): Promise<void> {
  for (const item of await db.getAll('queue')) {
    const next = { ...item, body: replaceId(item.body, from, to), path: replaceInPath(item.path, from, to) }
    if (JSON.stringify(next) !== JSON.stringify(item)) await db.put('queue', next)
  }
}

/** The person's record and every household that lists it move to the new id. */
export async function remapPersonId(db: Db, from: string, to: string): Promise<void> {
  const old = await db.get('people', from)
  if (old) {
    const known = await db.get('people', to) // the same person already cached from another phone
    await db.delete('people', from)
    await db.put('people', { ...old, ...known, id: to, pending: false })
  }
  for (const household of await db.getAll('households')) {
    const next = replaceId(household, from, to)
    if (JSON.stringify(next) !== JSON.stringify(household)) await db.put('households', next)
  }
  await remapQueuedIds(db, from, to)
}

const ID = '[^/]+'
const CHECKIN = new RegExp(`^/v1/shelters/${ID}/checkins$`)
const HOUSEHOLDS = new RegExp(`^/v1/shelters/${ID}/households$`)
const BIOMETRICS = new RegExp(`^/v1/people/(${ID})/biometrics$`)
const PERSON = new RegExp(`^/v1/people/(${ID})$`)
const HOUSEHOLD = new RegExp(`^/v1/households/(${ID})$`)
const CARDS = new RegExp(`^/v1/households/(${ID})/cards$`)
const CANCEL = new RegExp(`^/v1/cards/(${ID})/cancel$`)

async function patchPerson(db: Db, id: string, change: Partial<PersonRecord>) {
  const record = await db.get('people', id)
  if (record) await db.put('people', { ...record, ...change })
}

/** Called after a `2xx` of phase 3 items. The server's answer wins over what the phone assumed. */
export async function applyPhase3Synced(db: Db, item: QueueItem, body: unknown): Promise<void> {
  const reply = (body ?? {}) as { person_id?: string; level?: PersonRecord['level']; person?: Partial<PersonRecord> & { id?: string } }
  const body0 = (item.body ?? {}) as { id?: string }

  if (item.method === 'POST' && CHECKIN.test(item.path) && body0.id) {
    if (reply.person_id && reply.person_id !== body0.id) await remapPersonId(db, body0.id, reply.person_id)
    await patchPerson(db, reply.person_id ?? body0.id, { pending: false, ...(reply.level ? { level: reply.level } : {}) })
    return
  }
  const biometrics = item.method === 'PUT' ? BIOMETRICS.exec(item.path) : null
  if (biometrics) {
    const from = biometrics[1]!
    if (reply.person_id && reply.person_id !== from) await remapPersonId(db, from, reply.person_id)
    await patchPerson(db, reply.person_id ?? from, { ...(reply.level ? { level: reply.level } : {}) })
    return
  }
  const person = item.method === 'PATCH' ? PERSON.exec(item.path) : null
  if (person) {
    // An older edit comes back as `200` with the current state: show that, not what was typed.
    const { id: _id, ...state } = reply.person ?? {}
    await patchPerson(db, person[1]!, { ...pick(state), pending: false })
    return
  }
  if (item.method === 'POST' && HOUSEHOLDS.test(item.path) && body0.id) return markHousehold(db, body0.id)
  const household = item.method === 'PATCH' ? HOUSEHOLD.exec(item.path) : null
  if (household) return markHousehold(db, household[1]!)
  const card = item.method === 'POST' ? CARDS.exec(item.path) : null
  if (card) {
    const code = (item.body as { code?: string }).code
    const record = code ? await db.get('cards', code) : undefined
    if (record) await db.put('cards', { ...record, pending: false })
    return
  }
  const cancel = item.method === 'POST' ? CANCEL.exec(item.path) : null
  if (cancel) {
    const record = await db.get('cards', cancel[1]!)
    if (record) await db.put('cards', { ...record, status: 'canceled', pending: false })
  }
}

async function markHousehold(db: Db, id: string) {
  const record = await db.get('households', id)
  if (record) await db.put('households', { ...record, pending: false })
}

/** The fields of a `PersonRecord` that the server's view can overwrite. */
function pick(state: Partial<PersonRecord>): Partial<PersonRecord> {
  const keys = ['name', 'social_name', 'wristband', 'level', 'is_child', 'child_status', 'household_id', 'current_shelter_id', 'updated_at'] as const
  return Object.fromEntries(keys.filter((k) => state[k] !== undefined).map((k) => [k, state[k]]))
}
