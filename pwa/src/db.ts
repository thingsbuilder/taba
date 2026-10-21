import { type DBSchema, type IDBPDatabase, openDB } from 'idb'

// IndexedDB `taba` (spec 6.1.1). Version 1 created every store; version 2 (phase 3) adds the
// indexes the offline lists need and types the stores of people, households, cards and photos.

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
  /** Body of the error that held the item (the `shelter_nearby` conflict details). */
  detail?: unknown
  taken_over_from?: string
}

/** `shelters` store: the phone's copy of a shelter (spec 6.1.1). `pending` until the server confirmed the creation. */
export interface ShelterRecord {
  id: string
  name: string
  lat?: number
  lon?: number
  address?: { text?: string; city?: string; state?: string }
  capacity?: number | null
  confirmation?: 'unconfirmed' | 'confirmed'
  status?: 'open' | 'closed'
  event_name?: string | null
  pending?: boolean
}

/** `blobs` store: a photo waiting in the queue, always encrypted (spec 6.1.7). `iv` is the AES-GCM
 *  nonce; `data` the ciphertext. The key is `config.photo_key`, a non-extractable `CryptoKey`. */
export interface BlobRecord {
  queue_id: string
  iv: Uint8Array
  data: ArrayBuffer
  /** What the photo is for, so the queue knows which field of the request to fill. */
  kind?: 'checkin' | 'biometrics' | 'thumbnail'
}

/** `people` store: the shelter's people, for the offline list and search by name or wristband. */
export interface PersonRecord {
  id: string
  current_shelter_id?: string
  name?: string
  social_name?: string
  wristband?: string
  level?: 'verified' | 'assisted' | 'no_biometrics'
  is_child?: boolean
  child_status?: 'with_guardian' | 'separated' | 'unaccompanied'
  household_id?: string
  /** What the person screen needs to open and edit offline (spec 9.1). */
  birth_year?: number
  needs?: { continuous_medication: boolean; dietary_restriction: boolean; reduced_mobility: boolean; health_follow_up: boolean }
  vulnerabilities?: { pregnant: boolean; lactating: boolean; disability: ('mobility' | 'visual' | 'hearing' | 'intellectual' | 'psychosocial' | 'other')[]; indigenous_people?: string; migrant_language?: string }
  biometrics_declined?: boolean
  /** Made on this phone and not confirmed by the server yet. */
  pending?: boolean
  updated_at?: string
}

/** `households` store: a household and its members (offline). */
export interface HouseholdRecord {
  id: string
  shelter_id?: string
  member_ids: string[]
  head_id?: string
  pets?: { species: string; count: number }[]
  card_code?: string
  pending?: boolean
}

/** `cards` store: the cards of the current shelter's households (offline reading). */
export interface CardRecord {
  code: string
  household_id: string
  status: 'active' | 'canceled'
  pending?: boolean
}

export interface TabaDB extends DBSchema {
  config: { key: string; value: unknown }
  queue: { key: string; value: QueueItem }
  blobs: { key: string; value: BlobRecord }
  shelters: { key: string; value: ShelterRecord }
  people: { key: string; value: PersonRecord; indexes: { by_shelter: string; by_household: string } }
  households: { key: string; value: HouseholdRecord; indexes: { by_shelter: string } }
  cards: { key: string; value: CardRecord; indexes: { by_household: string } }
  captures: {
    key: string
    value: {
      capture_id: string
      created_at: number
      purpose?: 'invite' | 'checkin' | 'revalidation' | 'biometrics'
      invite_code?: string
      /** In-app path the journey returns to (check-in, revalidation and biometrics screens). */
      return_to?: string
    }
  }
  addresses: { key: string; value: { shelter_id: string; lat: number; lon: number; volunteer_id: string | null } }
}

export type Db = IDBPDatabase<TabaDB>

export const DB_NAME = 'taba'
export const DB_VERSION = 2

export function openTabaDb(name = DB_NAME): Promise<Db> {
  return openDB<TabaDB>(name, DB_VERSION, {
    upgrade(db, oldVersion, _newVersion, tx) {
      if (oldVersion < 1) {
        db.createObjectStore('config') // keyed by name
        db.createObjectStore('queue', { keyPath: 'id' })
        db.createObjectStore('blobs', { keyPath: 'queue_id' })
        db.createObjectStore('shelters', { keyPath: 'id' })
        db.createObjectStore('people', { keyPath: 'id' })
        db.createObjectStore('households', { keyPath: 'id' })
        db.createObjectStore('cards', { keyPath: 'code' })
        db.createObjectStore('captures', { keyPath: 'capture_id' })
        db.createObjectStore('addresses', { keyPath: 'shelter_id' })
      }
      if (oldVersion < 2) {
        tx.objectStore('people').createIndex('by_shelter', 'current_shelter_id')
        tx.objectStore('people').createIndex('by_household', 'household_id')
        tx.objectStore('households').createIndex('by_shelter', 'shelter_id')
        tx.objectStore('cards').createIndex('by_household', 'household_id')
      }
    },
  })
}
