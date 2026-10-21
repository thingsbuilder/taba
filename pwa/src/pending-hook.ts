import { useEffect, useState } from 'preact/hooks'
import type { Db, QueueItem } from './db'
import { type ConflictNames, conflictNames } from './pending'

/** Names for the conflicts among `items`, read from the phone's own copy whenever the items change. */
export function useConflictNames(db: Db, items: QueueItem[]): ConflictNames {
  const [names, setNames] = useState<ConflictNames>({ people: {}, households: {} })
  useEffect(() => {
    void conflictNames(db, items).then(setNames)
  }, [items])
  return names
}
