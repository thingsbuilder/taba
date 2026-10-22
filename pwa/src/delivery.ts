import type { Api } from './api'
import type { DeliveryReason, DeliveryRequest, DeliveryResponse, ItemSpec, Items, LivenessRequiredDetail } from './api-phase5'
import { errorKey } from './app-context'
import { finishCapture, pollCapture } from './captures'
import type { Db, HouseholdRecord, PersonRecord, QueueItem } from './db'
import { findLocalCard } from './households'
import { getLanguage } from './i18n'
import type { Queue } from './queue'
import { type LocalSession, roleIn } from './sessions'

// Delivery at the card (spec 3.5.3): the request, what waits for a person, the admin's approval and the
// result the queue brings back. The delivery goes through the queue; only the retry after a selfie is a direct call.

const PATH = /^\/v1\/shelters\/([^/]+)\/deliveries$/

export const deliveryPath = (shelterId: string) => `/v1/shelters/${shelterId}/deliveries`
export const isDelivery = (item: Pick<QueueItem, 'method' | 'path'>): boolean => item.method === 'POST' && PATH.test(item.path)

export type DeliveryBody = DeliveryRequest

export function buildDelivery(i: {
  id: string
  cardCode: string
  personId: string
  item: string
  quantity: number
  at: Date
  offline: boolean
  adminApproval?: boolean
}): DeliveryBody {
  return {
    id: i.id,
    card_code: i.cardCode,
    person_id: i.personId,
    item: i.item.trim(),
    quantity: i.quantity,
    delivered_at: i.at.toISOString(),
    ...(i.offline ? { offline: true as const } : {}),
    ...(i.adminApproval ? { admin_approval: true as const } : {}),
  }
}

export const enqueueDelivery = (queue: Pick<Queue, 'enqueue'>, who: { volunteer_id: string | null; shelter_id: string }, body: DeliveryBody) =>
  queue.enqueue({ method: 'POST', path: deliveryPath(who.shelter_id), body, ...who })

export type DeliveryFormError = 'item_required' | 'invalid_quantity'

export function validateDelivery(i: { item: string; quantity: string }): DeliveryFormError | null {
  if (i.item.trim() === '') return 'item_required'
  const n = Number(i.quantity)
  return Number.isInteger(n) && n >= 1 ? null : 'invalid_quantity'
}

// ---- errors ----

const LOCAL_ERRORS = new Set(['item_required', 'invalid_quantity', 'selfie_timeout', 'revalidation_failed'])
export const deliveryErrorKey = (code: string): string => (LOCAL_ERRORS.has(code) ? `delivery.error_${code}` : errorKey(code))

// ---- what waits for a person ----

/** The server asked for a selfie or an admin, or the selfie did not match: a selfie or an admin's approval can still settle the item. */
export const WAITING_ERRORS = new Set(['liveness_required', 'admin_approval_required', 'revalidation_failed'])

/** Whether a selfie can settle a waiting item: the server says `admin_approval_only` for a person without biometrics. */
export const selfieAllowed = (item: Pick<QueueItem, 'error' | 'detail'>): boolean =>
  item.error !== 'admin_approval_required' && (item.detail as Partial<LivenessRequiredDetail> | undefined)?.admin_approval_only !== true

/** Deliveries of the shelter that the queue stopped sending (any answer a repeat cannot fix); a person decides. */
export const waitingDeliveries = (items: QueueItem[], shelterId: string): QueueItem[] =>
  items.filter((i) => isDelivery(i) && i.shelter_id === shelterId && i.state === 'needs_action')

/** An admin of the shelter with a valid session on this device (the PIN switch is off, so it is whoever logged in here). */
export const adminOnDevice = (sessions: LocalSession[], shelterId: string): LocalSession | null =>
  sessions.find((s) => roleIn(s, shelterId) === 'admin') ?? null

/** Sends the waiting item again, with the admin's approval and the admin's session (spec 3.5.3 item 3). Same item, same key. */
export async function approveByAdmin(db: Db, queue: Pick<Queue, 'retry'>, itemId: string, admin: Pick<LocalSession, 'volunteer_id'>): Promise<void> {
  const item = await db.get('queue', itemId)
  if (!item) return
  await db.put('queue', { ...item, body: { ...(item.body as object), admin_approval: true }, volunteer_id: admin.volunteer_id })
  await queue.retry(itemId)
}

/** Offline or Unico out: a high value item cannot have a selfie, so an admin on the device is offered first. */
export const needsOfflineApproval = (spec: ItemSpec | undefined, withoutSelfie: boolean): boolean => withoutSelfie && spec?.high_value === true

// ---- the selfie ----

export interface DeliveryDraft {
  request: DeliveryBody
  /** The queue item that waits for this selfie, when there is one. */
  queue_id?: string
}

export type CaptureOutcome = { ok: true; reply: DeliveryResponse } | { ok: false; error: string }

/** Back from the selfie: the delivery goes out with the capture (waits for Unico), and the queue item that waited for it goes away. */
export async function sendWithCapture(
  deps: { db: Db; api: Api; queue: Pick<Queue, 'discard'> },
  secret: string,
  shelterId: string,
  captureId: string,
  draft: DeliveryDraft,
  sleep?: (ms: number) => Promise<void>,
): Promise<CaptureOutcome> {
  const res = await pollCapture(() => deps.api.postDelivery(secret, shelterId, { ...draft.request, capture_id: captureId }), sleep)
  await finishCapture(deps.db, captureId)
  if (!res.ok) {
    if (draft.queue_id && res.error === 'revalidation_failed') {
      const item = await deps.db.get('queue', draft.queue_id)
      if (item) await deps.db.put('queue', { ...item, error: 'revalidation_failed' })
    }
    return { ok: false, error: res.error === 'timeout' ? 'selfie_timeout' : res.error }
  }
  if (draft.queue_id) await deps.queue.discard(draft.queue_id)
  return { ok: true, reply: res.data }
}

// ---- the result ----

const replyKey = (queueId: string) => `delivery_reply:${queueId}`

/** Called by the queue after the `2xx` of a delivery, so the screen that enqueued it can show the result. */
export async function recordDeliveryReply(db: Db, item: QueueItem, reply: unknown): Promise<void> {
  const r = reply as Partial<DeliveryResponse> | null
  if (isDelivery(item) && r && typeof r.status === 'string') await db.put('config', { status: r.status, reasons: r.reasons ?? [], delivery_id: r.delivery_id }, replyKey(item.id))
}

/** The answer of a delivery the queue has sent, once; null while it has not. */
export async function takeDeliveryReply(db: Db, queueId: string): Promise<DeliveryResponse | null> {
  const reply = ((await db.get('config', replyKey(queueId))) as DeliveryResponse | undefined) ?? null
  if (reply) await db.delete('config', replyKey(queueId))
  return reply
}

const dateText = (iso: string) => new Date(iso).toLocaleDateString(getLanguage())

/** i18n key and values of the line for a `201`. The other shelter is named only when the API sent it (4.7). */
export function resultText(reply: Pick<DeliveryResponse, 'status' | 'reasons'>): { key: string; params: Record<string, string> } {
  if (reply.status !== 'in_review') return { key: 'delivery.done', params: {} }
  const dup: DeliveryReason | undefined = reply.reasons.find((r) => r.kind === 'duplicate_in_period')
  if (dup?.previous_delivered_at) {
    const date = dateText(dup.previous_delivered_at)
    return dup.previous_shelter_name ? { key: 'delivery.in_review_at', params: { date, shelter: dup.previous_shelter_name } } : { key: 'delivery.in_review', params: { date } }
  }
  if (reply.reasons.some((r) => r.kind === 'liveness_unavailable')) return { key: 'delivery.in_review_unavailable', params: {} }
  return { key: 'delivery.in_review_plain', params: {} }
}

// ---- items of the shelter ----

const itemsKey = (shelterId: string) => `shelter_items:${shelterId}`

/** `items` of `GET /v1/shelters/{id}`, kept for offline; the cached copy when there is no answer. */
export async function loadItems(db: Db, api: Api, secret: string, shelterId: string, online: boolean): Promise<Items> {
  if (online) {
    const res = await api.getShelter(secret, shelterId)
    if (res.ok) {
      const items = (res.data.shelter as { items?: Items }).items ?? {}
      await db.put('config', items, itemsKey(shelterId))
      return items
    }
  }
  return ((await db.get('config', itemsKey(shelterId))) as Items | undefined) ?? {}
}

// ---- the card ----

export interface CardMember {
  id: string
  name: string
  level?: PersonRecord['level']
}

export type CardOutcome = { ok: true; householdId: string; members: CardMember[] } | { ok: false; error: string }

/** The card read or typed: the server's view with signal (cached for later), the phone's copy without. Errors are i18n keys of the household screen. */
export async function resolveCard(db: Db, api: Api, secret: string, online: boolean, code: string, unnamed: string): Promise<CardOutcome> {
  const names: Record<string, CardMember> = {}
  let householdId: string
  let memberIds: string[]
  if (online) {
    const res = await api.getCard(secret, code)
    if (!res.ok) return { ok: false, error: res.status === 404 ? 'card.not_found' : errorKey(res.error) }
    if (res.data.status === 'canceled') return { ok: false, error: 'card.canceled' }
    householdId = res.data.household_id
    memberIds = res.data.members.map((m) => m.person_id)
    for (const m of res.data.members) names[m.person_id] = { id: m.person_id, name: m.name ?? '', level: m.level }
    await db.put('cards', { code, household_id: householdId, status: 'active' })
    if (!(await db.get('households', householdId))) await db.put('households', { id: householdId, member_ids: memberIds, card_code: code })
  } else {
    const local = await findLocalCard(db, code)
    if (!local) return { ok: false, error: 'card.offline_unknown' }
    if (local.card.status === 'canceled') return { ok: false, error: 'card.canceled' }
    householdId = local.card.household_id
    memberIds = (local.household as HouseholdRecord | undefined)?.member_ids ?? []
  }
  const members = await Promise.all(
    memberIds.map(async (id) => {
      const p = await db.get('people', id)
      return { id, name: p?.name || p?.wristband || names[id]?.name || unnamed, level: p?.level ?? names[id]?.level }
    }),
  )
  return { ok: true, householdId, members }
}
