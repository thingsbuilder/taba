import { type DBSchema, type IDBPDatabase, openDB } from 'idb'

// IndexedDB `taba`, version 1 (spec 6.1.1). Every store is created now so later phases
// need no migration.

export type QueueState = 'pending' | 'held' | 'needs_action'

export interface QueueItem {
  id: string
  method: string
  path: string
  body: unknown
  volunteer_id: string | null // null until login exists (phase 1)
  shelter_id?: string
  created_at: number // ms since epoch
  attempts: number
  next_attempt_at: number // ms since epoch
  state: QueueState
  error?: string
  taken_over_from?: string
}

export interface TabaDB extends DBSchema {
  config: { key: string; value: unknown }
  queue: { key: string; value: QueueItem }
  blobs: { key: string; value: { queue_id: string } }
  shelters: { key: string; value: { id: string } }
  people: { key: string; value: { id: string } }
  households: { key: string; value: { id: string } }
  cards: { key: string; value: { code: string } }
  captures: { key: string; value: { capture_id: string; created_at: number } }
  addresses: { key: string; value: { shelter_id: string } }
}

export type Db = IDBPDatabase<TabaDB>

export const DB_NAME = 'taba'
export const DB_VERSION = 1

export function openTabaDb(name = DB_NAME): Promise<Db> {
  return openDB<TabaDB>(name, DB_VERSION, {
    upgrade(db) {
      db.createObjectStore('config') // keyed by name
      db.createObjectStore('queue', { keyPath: 'id' })
      db.createObjectStore('blobs', { keyPath: 'queue_id' })
      db.createObjectStore('shelters', { keyPath: 'id' })
      db.createObjectStore('people', { keyPath: 'id' })
      db.createObjectStore('households', { keyPath: 'id' })
      db.createObjectStore('cards', { keyPath: 'code' })
      db.createObjectStore('captures', { keyPath: 'capture_id' })
      db.createObjectStore('addresses', { keyPath: 'shelter_id' })
    },
  })
}
