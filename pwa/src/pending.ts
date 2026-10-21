import { errorKey } from './app-context'
import type { QueueItem } from './db'

// What the "Pendências" list shows for a queue item (spec 9.2).

export interface PendingView {
  id: string
  /** i18n key of what the item does. */
  labelKey: string
  state: QueueItem['state']
  /** i18n key of the error, when the item waits for a person. */
  errorKey?: string
  shelterId?: string
}

function labelOf(item: QueueItem): string {
  if (item.method === 'POST' && item.path === '/v1/shelters') return 'pending.create_shelter'
  if (item.path.startsWith('/v1/shelters/')) return 'pending.update_shelter'
  if (item.path === '/v1/me') return 'pending.update_profile'
  return 'pending.other'
}

/** Items that wait for a person (`held` or `needs_action`), oldest first. */
export function pendingViews(items: QueueItem[], shelterId?: string): PendingView[] {
  return items
    .filter((i) => i.state !== 'pending' && (shelterId === undefined || i.shelter_id === shelterId))
    .map((i) => ({ id: i.id, labelKey: labelOf(i), state: i.state, errorKey: i.error ? errorKey(i.error) : undefined, shelterId: i.shelter_id }))
}
