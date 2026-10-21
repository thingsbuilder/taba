import type { ApiStatus } from './api'
import type { Db, QueueItem } from './db'

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

  async function enqueue(input: NewQueueItem): Promise<QueueItem> {
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
      if (item.id === failed.id || sameShelter) await db.put('queue', { ...item, state: 'held', error, detail })
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

  async function pass(): Promise<void> {
    let statusOk: boolean | undefined
    const degradedGateOpen = async () => (statusOk ??= (await deps.getStatus())?.unico === 'ok')

    for (const item of await sortedItems()) {
      if (item.state !== 'pending') continue // held / needs_action wait for a person
      if (item.error === 'unico_degraded') {
        // Retried only once GET /status is ok again; does not block the rest of the queue.
        if (!(await degradedGateOpen())) continue
      } else if (item.next_attempt_at > now()) {
        break // FIFO: later items may depend on this one
      }

      const secret = await deps.secretFor?.(item.volunteer_id)
      let res: Response
      try {
        res = await deps.fetch(item.path, {
          method: item.method,
          headers: {
            'Content-Type': 'application/json',
            'Idempotency-Key': item.id,
            'X-Device-Id': await deps.deviceId(),
            ...(secret ? { Authorization: `Bearer ${secret}` } : {}),
          },
          body: item.body === undefined ? undefined : JSON.stringify(item.body),
        })
      } catch {
        await scheduleRetry(item, 'network', null)
        break
      }

      if (res.ok) {
        const body = res.status === 204 ? null : await res.json().catch(() => null)
        await remove(item.id)
        await deps.onSynced?.(item, body)
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
      } else if (res.status === 503 && errorCode === 'unico_degraded') {
        await db.put('queue', { ...item, error: errorCode })
      } else if (res.status === 429 || res.status >= 500) {
        await scheduleRetry(item, errorCode ?? `http_${res.status}`, parseRetryAfter(res.headers.get('Retry-After'), now()))
        notify()
        break
      } else {
        await db.put('queue', { ...item, state: 'needs_action', error: errorCode ?? `http_${res.status}` })
      }
      notify()
    }
  }

  async function scheduleRetry(item: QueueItem, error: string, retryAfterMs: number | null): Promise<void> {
    const attempts = item.attempts + 1
    const delay = Math.max(backoffMs(attempts, random), retryAfterMs ?? 0)
    await db.put('queue', { ...item, attempts, next_attempt_at: now() + delay, error })
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
