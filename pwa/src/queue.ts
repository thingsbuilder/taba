import type { ApiStatus } from './api'
import { type PhotoFields, blobToBase64, decryptPhotoFields, encryptPhotoFields } from './crypto'
import type { Db, QueueItem } from './db'
import { logEvent, routeOf } from './diagnostics'
import { PHOTO_ERRORS } from './queue-errors'
import { recordSearchReply } from './search'
import { applyPhase3Synced, undoPhase3Local } from './sync'

// Generic offline write queue (spec 9.2). Strict FIFO, one item at a time.

export const BACKOFF_BASE_MS = 5_000
export const BACKOFF_MAX_MS = 10 * 60_000
export const DRAIN_INTERVAL_MS = 30_000

/** `min(5 s × 2^attempts, 10 min) × (0.5 to 1.5)`; `random` returns [0, 1). */
export function backoffMs(attempts: number, random: () => number): number {
  return Math.min(BACKOFF_BASE_MS * 2 ** attempts, BACKOFF_MAX_MS) * (0.5 + random())
}

/** `Retry-After` as a delay in ms: seconds or an HTTP date. Null when absent or unreadable. */
export function parseRetryAfter(value: string | null, now: number): number | null {
  if (value === null || value.trim() === '') return null
  const seconds = Number(value)
  if (Number.isFinite(seconds)) return Math.max(0, seconds * 1000)
  const date = Date.parse(value)
  return Number.isNaN(date) ? null : Math.max(0, date - now)
}

export interface QueueDeps {
  db: Db
  fetch: typeof fetch
  deviceId: () => Promise<string>
  getStatus: () => Promise<ApiStatus | null>
  /** Session secret of the volunteer who signed the item, for the `Authorization` header. */
  secretFor?: (volunteerId: string | null) => Promise<string | null>
  /** Called on `401`: the drain stops and the app asks for login. */
  onUnauthorized?: (volunteerId: string | null) => void
  /** Called after a `2xx` with the JSON body (null when empty), to refresh local caches. */
  onSynced?: (item: QueueItem, body: unknown) => Promise<void>
  now?: () => number
  random?: () => number
}

export type NewQueueItem = Pick<QueueItem, 'method' | 'path' | 'body' | 'volunteer_id' | 'shelter_id'>

/** Photos of a request. They are stored encrypted (spec 6.1.7) and put in the body as base64 only when sent. */
export interface QueuePhotos {
  photo?: Blob
  thumbnail?: Blob
  /** The photos of a search (`photos_base64`). */
  photos?: Blob[]
}

/** The two answers of a Unico that is out: the first request after the monthly limit, and the ones after it (spec 5.5, 5.7). */
const isUnicoOut = (code: string | undefined): boolean => code === 'unico_degraded' || code === 'unico_limit'

const CHECKIN_PATH = /^\/v1\/shelters\/[^/]+\/checkins$/
const RESCUE_PATH = '/v1/rescues'

/** Header that tells the server who signed the item before an admin took it over (spec 9.2, audit `taken_over_from`). */
export const TAKEN_OVER_HEADER = 'X-Taken-Over-From'

/** Ids a later item may depend on: the entity this item creates (`body.id`) and the person in a `/v1/people/{id}` path. */
function idsOf(item: QueueItem): string[] {
  const id = (item.body as { id?: unknown } | null)?.id
  const person = /^\/v1\/people\/([^/]+)/.exec(item.path)?.[1]
  return [typeof id === 'string' ? id : '', person ?? ''].filter(Boolean)
}

/** What the diagnostic log says about an item: the request, never its body. */
const describe = (item: QueueItem) => ({ method: item.method, route: routeOf(item.path), attempt: item.attempts + 1 })

const mentions = (item: QueueItem, ids: Set<string>): boolean => {
  if (ids.size === 0) return false
  const text = item.path + JSON.stringify(item.body ?? null)
  return [...ids].some((id) => text.includes(id))
}

export function createQueue(deps: QueueDeps) {
  const { db } = deps
  const now = deps.now ?? Date.now
  const random = deps.random ?? Math.random
  const listeners = new Set<() => void>()
  let inflight: Promise<void> | null = null
  let rerun = false
  let lastCreatedAt = 0

  const notify = () => listeners.forEach((l) => l())

  async function sortedItems(): Promise<QueueItem[]> {
    return (await db.getAll('queue')).sort((a, b) => a.created_at - b.created_at)
  }

  async function enqueue(input: NewQueueItem, photos?: QueuePhotos): Promise<QueueItem> {
    // Strictly increasing so FIFO order survives two enqueues in the same millisecond.
    lastCreatedAt = Math.max(now(), lastCreatedAt + 1)
    const item: QueueItem = {
      ...input,
      id: crypto.randomUUID(),
      created_at: lastCreatedAt,
      attempts: 0,
      next_attempt_at: 0,
      state: 'pending',
    }
    // The encrypted photo is stored first: the drain never sees an item whose photo is missing.
    if (photos?.photo || photos?.thumbnail || photos?.photos?.length) {
      const fields = {
        ...(photos.photo ? { photo_base64: await blobToBase64(photos.photo) } : {}),
        ...(photos.thumbnail ? { thumbnail_base64: await blobToBase64(photos.thumbnail) } : {}),
        ...(photos.photos?.length ? { photos_base64: await Promise.all(photos.photos.map(blobToBase64)) } : {}),
      }
      await db.put('blobs', await encryptPhotoFields(db, item.id, fields))
    }
    await db.put('queue', item)
    notify()
    void drain()
    return item
  }

  async function remove(id: string): Promise<void> {
    const tx = db.transaction(['queue', 'blobs'], 'readwrite')
    await Promise.all([tx.objectStore('queue').delete(id), tx.objectStore('blobs').delete(id), tx.done])
  }

  async function holdShelter(shelterId: string | undefined, error: string, failed: QueueItem, detail: unknown): Promise<void> {
    const items = await db.getAll('queue')
    for (const item of items) {
      const sameShelter = shelterId !== undefined && item.shelter_id === shelterId
      if (item.id === failed.id || sameShelter) {
        await db.put('queue', { ...item, state: 'held', error, detail })
        logEvent('queue_state', { ...describe(item), state: 'held', error })
      }
    }
  }

  /** The admin explained a shelter conflict (spec 3.2.1): amend the creation body and release what was held. */
  async function release(shelterId: string, patchBody: Record<string, unknown>): Promise<void> {
    for (const item of await sortedItems()) {
      if (item.shelter_id !== shelterId || item.state !== 'held') continue
      const isCreate = item.method === 'POST' && item.path === '/v1/shelters'
      const body = isCreate ? { ...(item.body as object), ...patchBody } : item.body
      const { error: _e, detail: _d, ...rest } = item
      await db.put('queue', { ...rest, body, state: 'pending', attempts: 0, next_attempt_at: 0 })
    }
    notify()
    await drain()
  }

  /** An admin takes the pending items of another volunteer in the given shelters (spec 9.2, M1): they go out with the admin's session. */
  async function takeOver(fromVolunteerId: string, toVolunteerId: string, shelterIds: string[]): Promise<number> {
    let n = 0
    for (const item of await sortedItems()) {
      if (item.volunteer_id !== fromVolunteerId || !item.shelter_id || !shelterIds.includes(item.shelter_id)) continue
      await db.put('queue', { ...item, volunteer_id: toVolunteerId, taken_over_from: item.taken_over_from ?? fromVolunteerId })
      n++
    }
    notify()
    await drain()
    return n
  }

  /**
   * A photo check-in or rescue queued while Unico worked, sent when it no longer does: it becomes the
   * registration without biometrics (`person_id` = its `id`) and a late biometrics `PUT` after it that
   * carries the photo, still encrypted (spec 5.7, 4.12). A rescue without a photo already is that; a
   * check-in says so with `no_biometrics`. False when the item is neither.
   */
  async function degradeCheckin(item: QueueItem): Promise<boolean> {
    const body = item.body as { id?: string; capture_id?: unknown; no_biometrics?: unknown } | null
    const photos = await db.get('blobs', item.id)
    const isCheckin = CHECKIN_PATH.test(item.path)
    if (item.method !== 'POST' || !(isCheckin || item.path === RESCUE_PATH) || !photos || !body?.id || body.capture_id || body.no_biometrics) return false
    const fields = await decryptPhotoFields(db, photos).catch(() => null)
    if (!fields?.photo_base64) return false

    const late: QueueItem = {
      id: crypto.randomUUID(),
      method: 'PUT',
      path: `/v1/people/${body.id}/biometrics`,
      body: {},
      volunteer_id: item.volunteer_id,
      shelter_id: item.shelter_id,
      created_at: Math.max(now(), lastCreatedAt + 1),
      attempts: 0,
      next_attempt_at: 0,
      state: 'pending',
      error: 'unico_degraded',
    }
    lastCreatedAt = late.created_at
    const { photo_base64, ...rest } = fields
    await db.put('blobs', await encryptPhotoFields(db, late.id, { photo_base64 }))
    if (rest.thumbnail_base64) await db.put('blobs', await encryptPhotoFields(db, item.id, rest))
    else await db.delete('blobs', item.id)
    await db.put('queue', late)
    const { error: _e, ...registration } = item
    await db.put('queue', { ...registration, body: isCheckin ? { ...(item.body as object), no_biometrics: true } : item.body, attempts: 0, next_attempt_at: 0 })
    notify()
    return true
  }

  /** A new photo for an item whose photo Unico refused: it goes out again with the same data and key. */
  async function replacePhoto(id: string, photo: Blob): Promise<void> {
    const item = await db.get('queue', id)
    if (!item) return
    const old = await db.get('blobs', id)
    const kept = old ? await decryptPhotoFields(db, old).catch(() => ({})) : {}
    await db.put('blobs', await encryptPhotoFields(db, id, { ...kept, photo_base64: await blobToBase64(photo) }))
    const { error: _e, detail: _d, ...rest } = item
    await db.put('queue', { ...rest, state: 'pending', attempts: 0, next_attempt_at: 0 })
    notify()
    await drain()
  }

  /**
   * Gives up the photo and registers the person by name or wristband, with the same data (path C, the
   * selfie may come later). A late biometrics item has nothing else to do and goes away.
   */
  async function registerWithoutPhoto(id: string): Promise<void> {
    const item = await db.get('queue', id)
    if (!item) return
    if (item.method === 'PUT') return discard(id)
    const old = await db.get('blobs', id)
    const kept = old ? await decryptPhotoFields(db, old).catch(() => ({}) as PhotoFields) : ({} as PhotoFields)
    const { photo_base64: _photo, ...rest } = kept
    if (rest.thumbnail_base64) await db.put('blobs', await encryptPhotoFields(db, id, rest))
    else await db.delete('blobs', id)
    const { error: _e, detail: _d, ...open } = item
    await db.put('queue', { ...open, body: { ...(item.body as object), no_biometrics: true }, state: 'pending', attempts: 0, next_attempt_at: 0 })
    notify()
    await drain()
  }

  /** Throws away an item that waits for a person (and its photo). */
  async function discard(id: string): Promise<void> {
    const item = await db.get('queue', id)
    if (item) await undoPhase3Local(db, item)
    if (item) logEvent('queue_discard', describe(item))
    await remove(id)
    notify()
  }

  /** Sends an item that waited for a person again, with the same idempotency key. */
  async function retry(id: string): Promise<void> {
    const item = await db.get('queue', id)
    if (!item) return
    const { error: _e, detail: _d, ...rest } = item
    await db.put('queue', { ...rest, state: 'pending', attempts: 0, next_attempt_at: 0 })
    notify()
    await drain()
  }

  async function pass(): Promise<void> {
    let statusOk: boolean | undefined
    const degradedGateOpen = async () => (statusOk ??= (await deps.getStatus())?.unico === 'ok')
    // Ids of items that wait for the status: whatever mentions them waits too (spec 9.3).
    const waiting = new Set<string>()

    for (const snapshot of await sortedItems()) {
      // Read again: an earlier item of this pass may have held this one (spec 9.3).
      const item = await db.get('queue', snapshot.id)
      if (!item || item.state !== 'pending') continue // held / needs_action wait for a person
      if (mentions(item, waiting)) continue
      if (isUnicoOut(item.error)) {
        // Retried only once GET /status is ok again; does not block the rest of the queue.
        if (!(await degradedGateOpen())) {
          idsOf(item).forEach((id) => waiting.add(id))
          continue
        }
      } else if (item.next_attempt_at > now()) {
        break // FIFO: later items may depend on this one
      }

      const secret = await deps.secretFor?.(item.volunteer_id)
      let body = item.body
      const photos = await db.get('blobs', item.id)
      if (photos) {
        try {
          body = { ...(item.body as object), ...(await decryptPhotoFields(db, photos)) }
        } catch {
          // The browser lost the key: the photo cannot be read any more, a person has to take it again.
          await db.put('queue', { ...item, state: 'needs_action', error: 'photo_unreadable' })
          logEvent('queue_state', { ...describe(item), state: 'needs_action', error: 'photo_unreadable' })
          notify()
          continue
        }
      }
      let res: Response
      try {
        res = await deps.fetch(item.path, {
          method: item.method,
          headers: {
            'Content-Type': 'application/json',
            'Idempotency-Key': item.id,
            'X-Device-Id': await deps.deviceId(),
            ...(secret ? { Authorization: `Bearer ${secret}` } : {}),
            ...(item.taken_over_from ? { [TAKEN_OVER_HEADER]: item.taken_over_from } : {}),
          },
          body: body === undefined ? undefined : JSON.stringify(body),
        })
      } catch {
        logEvent('queue_network_error', describe(item))
        await scheduleRetry(item, 'network', null)
        break
      }
      logEvent('queue_send', { ...describe(item), status: res.status })

      if (res.ok) {
        const reply = res.status === 204 ? null : await res.json().catch(() => null)
        await remove(item.id)
        await applyPhase3Synced(db, item, reply)
        await recordSearchReply(db, item, reply)
        await deps.onSynced?.(item, reply)
        notify()
        continue
      }
      if (res.status === 401) {
        deps.onUnauthorized?.(item.volunteer_id)
        break
      }
      const failure = (await res.json().catch(() => null)) as { error?: string; detail?: unknown } | null
      const errorCode = failure?.error

      if (res.status === 409 && errorCode === 'shelter_nearby') {
        await holdShelter(item.shelter_id, errorCode, item, failure?.detail)
      } else if (res.status === 503 && isUnicoOut(errorCode)) {
        if (await degradeCheckin(item)) {
          // Registered by path C now; the photo became a late biometrics item that waits (spec 5.7).
          rerun = true
          continue
        }
        await db.put('queue', { ...item, error: errorCode })
        idsOf(item).forEach((id) => waiting.add(id))
      } else if ((res.status === 429 && errorCode !== 'search_limit') || res.status >= 500) {
        // `search_limit` is the daily quota: repeating cannot help, a person decides (falls to `needs_action`).
        await scheduleRetry(item, errorCode ?? `http_${res.status}`, parseRetryAfter(res.headers.get('Retry-After'), now()))
        notify()
        break
      } else {
        // Conflicts of spec 9.4 (`person_in_other_household`, `household_has_card`, ...) keep what the server said.
        // A refused photo is never sent again as it is: the item keeps its data and waits for a person.
        const refused = errorCode !== undefined && PHOTO_ERRORS.has(errorCode)
        logEvent('queue_state', { ...describe(item), state: 'needs_action', error: errorCode ?? `http_${res.status}` })
        if (refused) logEvent('photo_rejected', { error: errorCode, refusals: (item.photo_refusals ?? 0) + 1 })
        await db.put('queue', {
          ...item,
          state: 'needs_action',
          error: errorCode ?? `http_${res.status}`,
          detail: failure?.detail,
          ...(refused ? { photo_refusals: (item.photo_refusals ?? 0) + 1 } : {}),
        })
      }
      notify()
    }
  }

  async function scheduleRetry(item: QueueItem, error: string, retryAfterMs: number | null): Promise<void> {
    const attempts = item.attempts + 1
    const delay = Math.max(backoffMs(attempts, random), retryAfterMs ?? 0)
    await db.put('queue', { ...item, attempts, next_attempt_at: now() + delay, error })
    logEvent('queue_retry', { method: item.method, route: routeOf(item.path), attempt: attempts, reason: error, delay_s: Math.round(delay / 1000) })
  }

  /** Single flight: a call during a drain schedules one more pass and resolves when it ends. */
  function drain(): Promise<void> {
    if (inflight) {
      rerun = true
      return inflight
    }
    inflight = (async () => {
      try {
        do {
          rerun = false
          await pass()
        } while (rerun)
      } finally {
        inflight = null
        notify()
      }
    })()
    return inflight
  }

  return {
    enqueue,
    drain,
    release,
    takeOver,
    discard,
    retry,
    replacePhoto,
    registerWithoutPhoto,
    list: sortedItems,
    count: () => db.count('queue'),
    subscribe(listener: () => void) {
      listeners.add(listener)
      return () => listeners.delete(listener)
    },
  }
}

export type Queue = ReturnType<typeof createQueue>

/** Spec 9.2 triggers: on open, on `online`, and every 30 s while the app is open (iOS has no Background Sync). */
export function startAutoDrain(queue: Queue, target: Pick<Window, 'addEventListener' | 'removeEventListener'>): () => void {
  const run = () => void queue.drain()
  run()
  target.addEventListener('online', run)
  const timer = setInterval(run, DRAIN_INTERVAL_MS)
  return () => {
    target.removeEventListener('online', run)
    clearInterval(timer)
  }
}
