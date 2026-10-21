import { describe, expect, it } from 'vitest'
import { openDB } from 'idb'
import { DB_VERSION, openTabaDb } from './db'
import { freshDb } from './testing'

describe('IndexedDB schema', () => {
  it('creates every store of spec 6.1.1', async () => {
    const db = await freshDb()
    expect(db.version).toBe(DB_VERSION)
    expect([...db.objectStoreNames].sort()).toEqual(
      ['addresses', 'blobs', 'captures', 'cards', 'config', 'households', 'people', 'queue', 'shelters'],
    )
  })
})

describe('version 2 (phase 3)', () => {
  it('adds the indexes of people, households and cards', async () => {
    const db = await freshDb()
    expect([...db.transaction('people').store.indexNames].sort()).toEqual(['by_household', 'by_shelter'])
    expect([...db.transaction('households').store.indexNames]).toEqual(['by_shelter'])
    expect([...db.transaction('cards').store.indexNames]).toEqual(['by_household'])
    await db.put('people', { id: 'p1', current_shelter_id: 's1', household_id: 'h1', name: 'Ana' })
    await db.put('people', { id: 'p2', current_shelter_id: 's2' })
    expect((await db.getAllFromIndex('people', 'by_shelter', 's1')).map((p) => p.id)).toEqual(['p1'])
  })

  it('upgrades a version 1 database keeping what was in it', async () => {
    const name = `taba-upgrade-${crypto.randomUUID()}`
    // The schema of version 1, as the phase 2 app created it.
    const v1 = await openDB(name, 1, {
      upgrade(d) {
        d.createObjectStore('config')
        d.createObjectStore('queue', { keyPath: 'id' })
        d.createObjectStore('blobs', { keyPath: 'queue_id' })
        d.createObjectStore('shelters', { keyPath: 'id' })
        d.createObjectStore('people', { keyPath: 'id' })
        d.createObjectStore('households', { keyPath: 'id' })
        d.createObjectStore('cards', { keyPath: 'code' })
        d.createObjectStore('captures', { keyPath: 'capture_id' })
        d.createObjectStore('addresses', { keyPath: 'shelter_id' })
      },
    })
    await v1.put('config', 'device-1', 'device_id')
    await v1.put('shelters', { id: 's1', name: 'Ginásio' })
    await v1.put('people', { id: 'p1', current_shelter_id: 's1' })
    v1.close()

    const db = await openTabaDb(name)
    expect(db.version).toBe(2)
    expect(await db.get('config', 'device_id')).toBe('device-1')
    expect((await db.get('shelters', 's1'))?.name).toBe('Ginásio')
    expect((await db.getAllFromIndex('people', 'by_shelter', 's1')).map((p) => p.id)).toEqual(['p1'])
  })
})
