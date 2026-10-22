import { deliveryQueueErrorKey, isDeliveryItem, phase3Label, queueErrorKey } from './queue-errors'
import type { Db, QueueItem } from './db'

// What the "Pendências" list shows for a queue item (spec 9.2).

export interface PendingView {
  id: string
  /** i18n key of what the item does. */
  labelKey: string
  state: QueueItem['state']
  /** i18n key of the error, when the item waits for a person. */
  errorKey?: string
  /** Where a person settles it, for the items that cannot be sent again as they are (a delivery waiting for a selfie or an admin). */
  openPath?: string
  /** Whether sending it again can help: the daily search limit cannot be beaten by waiting a minute. */
  retryable: boolean
  /** What the text of the error needs, such as which household got in the way. */
  errorParams?: Record<string, string>
  shelterId?: string
}

/** Names the phone knows, to say which person and which household a conflict is about. */
export interface ConflictNames {
  people: Record<string, string>
  /** Household id -> its members, as a text. */
  households: Record<string, string>
}

const NO_NAMES: ConflictNames = { people: {}, households: {} }

/** The text of the conflict of spec 9.4 with the household named, when the phone knows it. */
function conflictText(item: QueueItem, names: ConflictNames): { key: string; params: Record<string, string> } | null {
  const detail = (item.detail ?? {}) as { person_id?: string; household_id?: string }
  if (item.error === 'person_in_other_household') {
    const household = detail.household_id ? names.households[detail.household_id] : undefined
    if (!household) return null
    return { key: 'queue.error_person_in_other_household_named', params: { person: (detail.person_id && names.people[detail.person_id]) || '?', household } }
  }
  if (item.error === 'household_has_card') {
    const own = /^\/v1\/households\/([^/]+)\/cards$/.exec(item.path)?.[1]
    const household = own ? names.households[own] : undefined
    return household ? { key: 'queue.error_household_has_card_named', params: { household } } : null
  }
  return null
}

/** Who and which household the waiting conflicts are about, from the phone's own copy. */
export async function conflictNames(db: Db, items: QueueItem[]): Promise<ConflictNames> {
  const names: ConflictNames = { people: {}, households: {} }
  const householdIds = new Set<string>()
  const personIds = new Set<string>()
  for (const item of items.filter((i) => i.state === 'needs_action')) {
    const detail = (item.detail ?? {}) as { person_id?: string; household_id?: string }
    if (detail.household_id) householdIds.add(detail.household_id)
    if (detail.person_id) personIds.add(detail.person_id)
    const own = /^\/v1\/households\/([^/]+)\/cards$/.exec(item.path)?.[1]
    if (own) householdIds.add(own)
  }
  const personName = async (id: string) => {
    const person = await db.get('people', id)
    return person?.name || person?.wristband || undefined
  }
  for (const id of personIds) {
    const name = await personName(id)
    if (name) names.people[id] = name
  }
  for (const id of householdIds) {
    const household = await db.get('households', id)
    const members = (await Promise.all((household?.member_ids ?? []).map(personName))).filter(Boolean)
    if (members.length > 0) names.households[id] = members.join(', ')
  }
  return names
}

function labelOf(item: QueueItem): string {
  const phase3 = phase3Label(item)
  if (phase3) return phase3
  if (item.method === 'POST' && item.path === '/v1/shelters') return 'pending.create_shelter'
  if (item.path.startsWith('/v1/shelters/')) return 'pending.update_shelter'
  if (item.path === '/v1/me') return 'pending.update_profile'
  return 'pending.other'
}

/** Items that wait for a person (`held` or `needs_action`), oldest first. */
export function pendingViews(items: QueueItem[], shelterId?: string, names: ConflictNames = NO_NAMES): PendingView[] {
  return items
    .filter((i) => i.state !== 'pending' && (shelterId === undefined || i.shelter_id === shelterId))
    .map((i) => {
      const conflict = conflictText(i, names)
      return {
        id: i.id,
        labelKey: labelOf(i),
        state: i.state,
        errorKey: conflict?.key ?? (i.error ? (isDeliveryItem(i) ? deliveryQueueErrorKey(i.error) : queueErrorKey(i.error)) : undefined),
        retryable: i.error !== 'search_limit' && !isDeliveryItem(i),
        ...(isDeliveryItem(i) && i.shelter_id ? { openPath: `/shelter/${i.shelter_id}/delivery` } : {}),
        ...(conflict ? { errorParams: conflict.params } : {}),
        shelterId: i.shelter_id,
      }
    })
}
