import type { FamilyLanguage, NearbyShelter, SearchRequest, SearchResponse } from './api-phase4'
import type { Db, QueueItem } from './db'
import { type Frame, photoForToken } from './image'
import type { Queue } from './queue'
import type { LocalSession } from './sessions'

// Search for a missing person (spec 3.7): the form, the photos and the queue item.

export const SEARCH_PATH = '/v1/searches'
export const MAX_PHOTOS = 5
export const FAMILY_LANGUAGES: FamilyLanguage[] = ['pt-BR', 'en', 'es']

export interface SearchForm {
  name: string
  nickname: string
  notes: string
  family_name: string
  phone: string
  relationship: string
  language: FamilyLanguage
}

export const emptySearchForm = (): SearchForm => ({ name: '', nickname: '', notes: '', family_name: '', phone: '', relationship: '', language: 'pt-BR' })

export type SearchError = 'name' | 'family_name' | 'phone' | 'relationship' | 'photos'

/** Everything about the missing person and the family, but the notes and the nickname, and 1 to 5 photos (spec 3.7 item 3). */
export function validateSearch(form: SearchForm, photoCount: number): SearchError[] {
  const errors: SearchError[] = []
  if (form.name.trim() === '') errors.push('name')
  if (form.family_name.trim() === '') errors.push('family_name')
  if (form.phone.trim() === '') errors.push('phone')
  if (form.relationship.trim() === '') errors.push('relationship')
  if (photoCount < 1 || photoCount > MAX_PHOTOS) errors.push('photos')
  return errors
}

/** The request without photos: the queue puts `photos_base64` in when it sends (they wait encrypted). */
export function searchBody(id: string, form: SearchForm): Omit<SearchRequest, 'photos_base64'> {
  const nickname = form.nickname.trim()
  return {
    id,
    missing_person: { name: form.name.trim(), ...(nickname ? { nickname } : {}), notes: form.notes.trim() },
    family: { name: form.family_name.trim(), phone: form.phone.trim(), relationship: form.relationship.trim(), language: form.language },
  }
}

export const enqueueSearch = (queue: Pick<Queue, 'enqueue'>, i: { id: string; form: SearchForm; photos: Blob[]; volunteerId: string | null }): Promise<QueueItem> =>
  queue.enqueue({ method: 'POST', path: SEARCH_PATH, body: searchBody(i.id, i.form), volunteer_id: i.volunteerId }, { photos: i.photos })

async function fileToFrame(file: Blob): Promise<Frame> {
  const bitmap = await createImageBitmap(file)
  return { source: bitmap, width: bitmap.width, height: bitmap.height }
}

/** Gallery or camera pictures reduced to under 800 KB each (spec 6.1.4). Null when one cannot get that small. */
export async function reducePhotos(files: Blob[], toFrame: (file: Blob) => Promise<Frame> = fileToFrame, reduce = photoForToken): Promise<Blob[] | null> {
  const out: Blob[] = []
  for (const file of files) {
    const reduced = await reduce(await toFrame(file))
    if (!reduced) return null
    out.push(reduced)
  }
  return out
}

// The `usable_photos` of the answer reaches the screen through the phone's own storage: the queue
// sends the search, possibly long after the screen that enqueued it is gone.

const replyKey = (searchId: string) => `search_reply:${searchId}`

/** Called by the queue after the `2xx` of a search. */
export async function recordSearchReply(db: Db, item: QueueItem, reply: unknown): Promise<void> {
  const id = (item.body as { id?: unknown } | null)?.id
  const usable = (reply as Partial<SearchResponse> | null)?.usable_photos
  if (item.method === 'POST' && item.path === SEARCH_PATH && typeof id === 'string' && typeof usable === 'number') await db.put('config', usable, replyKey(id))
}

/** `usable_photos` of a search the queue has sent, once; null while it has not. */
export async function takeSearchReply(db: Db, searchId: string): Promise<number | null> {
  const usable = await db.get('config', replyKey(searchId))
  if (typeof usable !== 'number') return null
  await db.delete('config', replyKey(searchId))
  return usable
}

/** Global roles that register searches without belonging to a confirmed shelter (spec 3.7 item 1). */
const SEARCH_ROLES = ['civil_defense', 'mediator', 'protection']

export const confirmedShelters = (session: Pick<LocalSession, 'memberships'>) => (session.memberships ?? []).filter((m) => m.confirmation === 'confirmed')

/** Whether the volunteer searches by a global role and so may name any confirmed shelter for a family link. */
export const hasSearchRole = (session: Pick<LocalSession, 'global_roles'>): boolean => session.global_roles.some((r) => SEARCH_ROLES.includes(r))

export interface ShelterChoice {
  shelter_id: string
  name: string
}

/**
 * The shelters a family link can come from: the volunteer's own confirmed ones first (the default),
 * then the confirmed ones found near them (`GET /v1/shelters/nearby`); no shelter twice.
 */
export function shelterChoices(own: ShelterChoice[], near: Pick<NearbyShelter, 'id' | 'name' | 'confirmation'>[]): ShelterChoice[] {
  const found = near.filter((s) => s.confirmation === 'confirmed').map((s) => ({ shelter_id: s.id, name: s.name }))
  return [...own, ...found.filter((s) => !own.some((o) => o.shelter_id === s.shelter_id))]
}

export const canSearch = (session: Pick<LocalSession, 'memberships' | 'global_roles'>): boolean =>
  confirmedShelters(session).length > 0 || session.global_roles.some((r) => SEARCH_ROLES.includes(r))

export type ShareResult = 'shared' | 'copied' | 'canceled' | 'unsupported'

/** The phone's share sheet; without it, copies the text. */
export async function shareLink(
  nav: { share?: (data: { text: string; url: string }) => Promise<void>; clipboard?: { writeText: (text: string) => Promise<void> } },
  text: string,
  url: string,
): Promise<ShareResult> {
  if (nav.share) {
    try {
      await nav.share({ text, url })
      return 'shared'
    } catch (e) {
      if ((e as { name?: string } | null)?.name === 'AbortError') return 'canceled'
    }
  }
  if (!nav.clipboard) return 'unsupported'
  try {
    await nav.clipboard.writeText(`${text} ${url}`)
    return 'copied'
  } catch {
    return 'unsupported'
  }
}
