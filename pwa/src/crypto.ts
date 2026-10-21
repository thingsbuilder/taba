import type { BlobRecord, Db } from './db'

// Photos that wait in the queue are stored encrypted (spec 6.1.7): AES-GCM with a `photo_key`
// created on first use as a non-extractable `CryptoKey` and kept in `config.photo_key`.

const KEY_NAME = 'photo_key'
const IV_BYTES = 12

/** The phone's photo key; created on first use. Safe against two callers racing on first use. */
export async function getPhotoKey(db: Db): Promise<CryptoKey> {
  const existing = await db.get('config', KEY_NAME)
  if (existing) return existing as CryptoKey
  const fresh = await crypto.subtle.generateKey({ name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt'])
  // Read again inside one transaction: the first writer wins, so a blob never meets a key that was replaced.
  const tx = db.transaction('config', 'readwrite')
  const winner = (await tx.store.get(KEY_NAME)) as CryptoKey | undefined
  if (!winner) await tx.store.put(fresh, KEY_NAME)
  await tx.done
  return winner ?? fresh
}

/** Encrypts `bytes` with a fresh random nonce. */
export async function encryptBytes(db: Db, queueId: string, bytes: Uint8Array): Promise<BlobRecord> {
  const iv = crypto.getRandomValues(new Uint8Array(IV_BYTES))
  const data = await crypto.subtle.encrypt({ name: 'AES-GCM', iv }, await getPhotoKey(db), bytes as BufferSource)
  return { queue_id: queueId, iv, data }
}

/** Throws when the key or the data are not the ones that encrypted it. */
export async function decryptBytes(db: Db, record: BlobRecord): Promise<Uint8Array> {
  const plain = await crypto.subtle.decrypt({ name: 'AES-GCM', iv: record.iv as BufferSource }, await getPhotoKey(db), record.data)
  return new Uint8Array(plain)
}

/** The secret fields of a request (the photos), sent as base64 text in the body once decrypted. */
export interface PhotoFields {
  photo_base64?: string
  thumbnail_base64?: string
}

const encoder = new TextEncoder()
const decoder = new TextDecoder()

export const encryptPhotoFields = (db: Db, queueId: string, fields: PhotoFields): Promise<BlobRecord> =>
  encryptBytes(db, queueId, encoder.encode(JSON.stringify(fields)))

export async function decryptPhotoFields(db: Db, record: BlobRecord): Promise<PhotoFields> {
  return JSON.parse(decoder.decode(await decryptBytes(db, record))) as PhotoFields
}

/** Base64 of a photo `Blob`, without the `data:` prefix (what `photo_base64` takes). */
export async function blobToBase64(blob: Blob): Promise<string> {
  const bytes = new Uint8Array(await blob.arrayBuffer())
  let binary = ''
  for (let i = 0; i < bytes.length; i += 0x8000) binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000))
  return btoa(binary)
}
