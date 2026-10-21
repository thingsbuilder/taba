import type { NearbyShelter, RescueRequest } from './api-phase4'
import { needsThumbnail } from './checkin'
import type { QueueItem } from './db'
import type { Position } from './geo'
import type { Queue } from './queue'

// Rescue at the landing point (spec 3.6): the request and what goes in the queue.

export const RESCUE_PATH = '/v1/rescues'

export interface RescueForm {
  name: string
  wristband: string
  note: string
  landing_point: string
}

export const emptyRescueForm = (): RescueForm => ({ name: '', wristband: '', note: '', landing_point: '' })

export type RescueError = 'position' | 'name_or_wristband'

/** The point is always recorded. Without a photo the name or the wristband is all there is to find the person again (spec 3.4, path C). */
export function validateRescue(form: RescueForm, i: { position: Position | null; hasPhoto: boolean }): RescueError[] {
  const errors: RescueError[] = []
  if (!i.position) errors.push('position')
  if (!i.hasPhoto && form.name.trim() === '' && form.wristband.trim() === '') errors.push('name_or_wristband')
  return errors
}

/** The request without photos; `destination` is the shelter picked from the nearby list. */
export function rescueBody(i: { id: string; form: RescueForm; position: Position; capturedAt: Date; destination?: string }): RescueRequest {
  const text = (v: string) => v.trim() || undefined
  const data = { name: text(i.form.name), wristband: text(i.form.wristband), note: text(i.form.note) }
  const hasData = Object.values(data).some((v) => v !== undefined)
  return {
    id: i.id,
    lat: i.position.lat,
    lon: i.position.lon,
    accuracy: i.position.accuracy,
    captured_at: i.capturedAt.toISOString(),
    ...(hasData ? { data: Object.fromEntries(Object.entries(data).filter(([, v]) => v !== undefined)) } : {}),
    ...(text(i.form.landing_point) ? { landing_point: text(i.form.landing_point) } : {}),
    ...(i.destination ? { destination_shelter_id: i.destination } : {}),
  }
}

/**
 * What to put in the queue (spec 3.6 item 6, 5.7). With the photo and Unico working, one `POST /v1/rescues`
 * carries the encrypted photo. With Unico down the rescue goes without it (the person is `no_biometrics`,
 * `person_id` = `id`) and the photo waits as a late biometrics `PUT` that the queue sends after it. The
 * thumbnail is only kept for a person without name and wristband (spec 4.6).
 */
export async function enqueueRescue(
  queue: Pick<Queue, 'enqueue'>,
  i: { body: RescueRequest; photo?: Blob; thumbnail?: Blob; degraded: boolean; volunteerId: string | null },
): Promise<QueueItem[]> {
  const who = { volunteer_id: i.volunteerId }
  const thumbnail = i.photo && i.thumbnail && needsThumbnail(i.body.data ?? {}) ? i.thumbnail : undefined
  if (i.photo && i.degraded) {
    return [
      await queue.enqueue({ method: 'POST', path: RESCUE_PATH, body: i.body, ...who }, thumbnail ? { thumbnail } : undefined),
      await queue.enqueue({ method: 'PUT', path: `/v1/people/${i.body.id}/biometrics`, body: {}, ...who }, { photo: i.photo }),
    ]
  }
  const blobs = { ...(i.photo ? { photo: i.photo } : {}), ...(thumbnail ? { thumbnail } : {}) }
  return [await queue.enqueue({ method: 'POST', path: RESCUE_PATH, body: i.body, ...who }, Object.keys(blobs).length > 0 ? blobs : undefined)]
}

/** i18n key and values of the line that says how many places a shelter has left. */
export function vacancyText(s: Pick<NearbyShelter, 'capacity' | 'occupancy' | 'vacancies'>): { key: string; params: Record<string, number> } {
  if (s.capacity === null || s.vacancies === null) return { key: 'rescue.shelter_no_capacity', params: { occupancy: s.occupancy } }
  if (s.vacancies <= 0) return { key: 'rescue.shelter_full', params: { occupancy: s.occupancy, capacity: s.capacity } }
  return { key: 'rescue.shelter_vacancies', params: { vacancies: s.vacancies, occupancy: s.occupancy, capacity: s.capacity } }
}

/** Meters under a kilometer, kilometers with one decimal above. */
export const distanceText = (m: number): { key: string; params: Record<string, number> } =>
  m < 1000 ? { key: 'rescue.distance_m', params: { n: Math.round(m) } } : { key: 'rescue.distance_km', params: { n: Math.round(m / 100) / 10 } }
