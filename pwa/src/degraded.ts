import type { ApiStatus } from './api'
import type { CheckinRequest } from './api-phase3'
import type { QueueItem } from './db'
import type { Queue } from './queue'

type Enqueuer = Pick<Queue, 'enqueue'>

// Degraded mode (spec 5.7): without the Unico the check-in goes by path C, and the photo, if any,
// waits encrypted in the queue as a late biometrics item.

export const isDegraded = (status: ApiStatus | null): boolean => status?.unico === 'degraded'

export type DegradedCheckin = Omit<CheckinRequest, 'capture_id' | 'photo_base64' | 'no_biometrics'>

/**
 * Two queue items, in order: the check-in without biometrics (`person_id` = `checkin.id`), then
 * `PUT /v1/people/{id}/biometrics` carrying the photo. The second one waits for `GET /status` to be
 * `ok` (it answers `503 unico_degraded` until then) and never holds the first back.
 */
export async function enqueueDegradedCheckin(
  queue: Enqueuer,
  checkin: DegradedCheckin,
  photo: Blob | undefined,
  who: { volunteer_id: string | null; shelter_id: string },
  /** The 128x128 thumbnail of a person with no name or wristband; it goes with the check-in. */
  thumbnail?: Blob,
): Promise<QueueItem[]> {
  const items = [
    await queue.enqueue({ method: 'POST', path: `/v1/shelters/${who.shelter_id}/checkins`, body: { ...checkin, no_biometrics: true }, ...who }, thumbnail ? { thumbnail } : undefined),
  ]
  if (photo) {
    items.push(await queue.enqueue({ method: 'PUT', path: `/v1/people/${checkin.id}/biometrics`, body: {}, ...who }, { photo }))
  }
  return items
}
