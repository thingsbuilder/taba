import { describe, expect, it } from 'vitest'
import { blobToBase64, decryptBytes, decryptPhotoFields, encryptBytes, encryptPhotoFields, getPhotoKey } from './crypto'
import { freshDb } from './testing'

const JPEG = new Uint8Array([0xff, 0xd8, 0xff, 0xe0, 0, 0x10, 0x4a, 0x46, 0x49, 0x46, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9])

const contains = (haystack: Uint8Array, needle: Uint8Array) => {
  for (let i = 0; i + needle.length <= haystack.length; i++) if (needle.every((b, j) => haystack[i + j] === b)) return true
  return false
}

describe('photo key', () => {
  it('is a non-extractable AES-GCM key kept in config.photo_key and reused', async () => {
    const db = await freshDb()
    const key = await getPhotoKey(db)
    expect(key.extractable).toBe(false)
    expect(key.algorithm).toMatchObject({ name: 'AES-GCM', length: 256 })
    await expect(crypto.subtle.exportKey('raw', key)).rejects.toThrow()

    expect(await db.get('config', 'photo_key')).toBeTruthy()
    const record = await encryptBytes(db, 'q', JPEG)
    expect(await decryptBytes(db, record)).toEqual(JPEG) // the stored key still decrypts
  })

  it('first use by two callers at once ends with a single key', async () => {
    const db = await freshDb()
    const records = await Promise.all([encryptBytes(db, 'a', JPEG), encryptBytes(db, 'b', JPEG)])
    for (const r of records) expect(await decryptBytes(db, r)).toEqual(JPEG)
  })
})

describe('encrypted photo fields', () => {
  it('round-trips the photo and the thumbnail', async () => {
    const db = await freshDb()
    const fields = { photo_base64: await blobToBase64(new Blob([JPEG])), thumbnail_base64: 'dGh1bWI=' }
    expect(await decryptPhotoFields(db, await encryptPhotoFields(db, 'q', fields))).toEqual(fields)
  })

  it('the stored bytes are not a readable JPEG, nor its base64', async () => {
    const db = await freshDb()
    const record = await encryptPhotoFields(db, 'q', { photo_base64: await blobToBase64(new Blob([JPEG])) })
    const stored = new Uint8Array(record.data)
    expect(Array.from(stored.subarray(0, 3))).not.toEqual([0xff, 0xd8, 0xff])
    expect(contains(stored, JPEG)).toBe(false)
    expect(contains(stored, new TextEncoder().encode('/9j/'))).toBe(false) // base64 of the JPEG header
  })

  it('uses a new nonce on every encryption', async () => {
    const db = await freshDb()
    const nonces = new Set<string>()
    for (let i = 0; i < 20; i++) nonces.add((await encryptBytes(db, 'q', JPEG)).iv.join(','))
    expect(nonces.size).toBe(20)
  })

  it('cannot be decrypted with another key', async () => {
    const db = await freshDb()
    const record = await encryptBytes(db, 'q', JPEG)
    await db.delete('config', 'photo_key') // the browser lost the key: the photo is gone, not readable
    await expect(decryptBytes(db, record)).rejects.toThrow()
  })
})
