import { type Db, openTabaDb } from './db'

/** A fresh in-memory IndexedDB per test (fake-indexeddb is installed globally by vitest.setupFiles). */
export function freshDb(): Promise<Db> {
  return openTabaDb(`taba-test-${crypto.randomUUID()}`)
}
