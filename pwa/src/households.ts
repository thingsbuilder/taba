import type { HouseholdPatchRequest, Pet } from './api-phase3'
import { generateCardCode } from './card-code'
import type { CardRecord, Db, HouseholdRecord } from './db'
import type { Queue } from './queue'

// Households and cards on the phone (spec 3.5.1): the change is made in the local copy at once and
// goes to the server through the queue, so it works offline (spec 9.1).

export interface Who {
  volunteer_id: string | null
  shelter_id: string
}

const nowIso = () => new Date().toISOString()

/** Where each member of the household points to, in the people cache. */
async function pointPeople(db: Db, ids: string[], householdId: string | undefined) {
  for (const id of ids) {
    const person = await db.get('people', id)
    if (person) await db.put('people', { ...person, household_id: householdId })
  }
}

export async function createHousehold(
  db: Db,
  queue: Queue,
  who: Who,
  input: { id: string; member_ids: string[]; head_id?: string; pets?: Pet[] },
): Promise<HouseholdRecord> {
  const record: HouseholdRecord = { id: input.id, shelter_id: who.shelter_id, member_ids: input.member_ids, head_id: input.head_id, pets: input.pets ?? [], pending: true }
  await db.put('households', record)
  await pointPeople(db, record.member_ids, record.id)
  await queue.enqueue(
    {
      method: 'POST',
      path: `/v1/shelters/${who.shelter_id}/households`,
      body: { id: input.id, member_ids: input.member_ids, ...(input.head_id ? { head_id: input.head_id } : {}), ...(input.pets?.length ? { pets: input.pets } : {}), client_created_at: nowIso() },
      ...who,
    },
  )
  return record
}

export async function updateHousehold(db: Db, queue: Queue, who: Who, id: string, patch: HouseholdPatchRequest): Promise<HouseholdRecord | null> {
  const record = await db.get('households', id)
  if (!record) return null
  const removed = new Set(patch.remove_member_ids ?? [])
  const members = [...record.member_ids.filter((m) => !removed.has(m)), ...(patch.add_member_ids ?? []).filter((m) => !record.member_ids.includes(m))]
  const head = patch.head_id ?? (record.head_id && removed.has(record.head_id) ? undefined : record.head_id)
  const next: HouseholdRecord = { ...record, member_ids: members, head_id: head, pets: patch.pets ?? record.pets, pending: true }
  await db.put('households', next)
  await pointPeople(db, [...removed], undefined)
  await pointPeople(db, patch.add_member_ids ?? [], id)
  await queue.enqueue({ method: 'PATCH', path: `/v1/households/${id}`, body: patch, ...who })
  return next
}

async function putCard(db: Db, household: HouseholdRecord, code: string) {
  await db.put('cards', { code, household_id: household.id, status: 'active', pending: true })
  await db.put('households', { ...household, card_code: code })
}

/** Issues the card of a household that has none: a new code, made here, shown as a QR (spec 3.5.1). */
export async function issueCard(db: Db, queue: Queue, who: Who, household: HouseholdRecord, code = generateCardCode()): Promise<string> {
  await putCard(db, household, code)
  await queue.enqueue({ method: 'POST', path: `/v1/households/${household.id}/cards`, body: { code, client_issued_at: nowIso() }, ...who })
  return code
}

/**
 * Cancels the card (lost, damaged) and, when asked, issues the substitute. The two go to the queue in
 * this order, because a household has only one active card.
 */
export async function cancelCard(db: Db, queue: Queue, who: Who, household: HouseholdRecord, code: string, substitute: boolean): Promise<string | null> {
  const old = await db.get('cards', code)
  if (old) await db.put('cards', { ...old, status: 'canceled', pending: true })
  const replacement = substitute ? generateCardCode() : null
  await queue.enqueue({ method: 'POST', path: `/v1/cards/${code}/cancel`, body: replacement ? { replaced_by: replacement } : {}, ...who })
  if (replacement) return issueCard(db, queue, who, household, replacement)
  const { card_code: _gone, ...rest } = household
  await db.put('households', rest)
  return null
}

export interface LocalCard {
  card: CardRecord
  household: HouseholdRecord | undefined
}

/** Reads a card with no signal: the phone's copy of the cards of the shelter (spec 6.1.1). */
export async function findLocalCard(db: Db, code: string): Promise<LocalCard | null> {
  const card = await db.get('cards', code)
  return card ? { card, household: await db.get('households', card.household_id) } : null
}
