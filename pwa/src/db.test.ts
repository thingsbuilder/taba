import { describe, expect, it } from 'vitest'
import { DB_VERSION } from './db'
import { freshDb } from './testing'

describe('IndexedDB schema', () => {
  it('creates every store of spec 6.1.1 in version 1', async () => {
    const db = await freshDb()
    expect(db.version).toBe(DB_VERSION)
    expect([...db.objectStoreNames].sort()).toEqual(
      ['addresses', 'blobs', 'captures', 'cards', 'config', 'households', 'people', 'queue', 'shelters'],
    )
  })
})
