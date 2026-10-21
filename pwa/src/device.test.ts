import { describe, expect, it, vi } from 'vitest'
import { getOrCreateDeviceId, isIosOutsideStandalone, requestPersistence } from './device'
import { freshDb } from './testing'

const IPHONE = 'Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Mobile/15E148 Safari/604.1'
const IPHONE_CHROME = 'Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/124.0 Mobile/15E148 Safari/604.1'
const ANDROID = 'Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Mobile Safari/537.36'
const MAC = 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Safari/605.1.15'

describe('isIosOutsideStandalone', () => {
  it('blocks iPhone Safari in a tab', () => {
    expect(isIosOutsideStandalone({ userAgent: IPHONE, platform: 'iPhone', maxTouchPoints: 5, standalone: false })).toBe(true)
  })
  it('blocks when navigator.standalone is undefined (iOS browsers outside the home screen)', () => {
    expect(isIosOutsideStandalone({ userAgent: IPHONE_CHROME, platform: 'iPhone', maxTouchPoints: 5 })).toBe(true)
  })
  it('lets the installed iPhone app through', () => {
    expect(isIosOutsideStandalone({ userAgent: IPHONE, platform: 'iPhone', maxTouchPoints: 5, standalone: true })).toBe(false)
  })
  it('blocks iPadOS reporting itself as a Mac', () => {
    expect(isIosOutsideStandalone({ userAgent: MAC, platform: 'MacIntel', maxTouchPoints: 5, standalone: false })).toBe(true)
  })
  it('never blocks Android or desktop', () => {
    expect(isIosOutsideStandalone({ userAgent: ANDROID, platform: 'Linux armv8l', maxTouchPoints: 5 })).toBe(false)
    expect(isIosOutsideStandalone({ userAgent: MAC, platform: 'MacIntel', maxTouchPoints: 0 })).toBe(false)
  })
})

describe('getOrCreateDeviceId', () => {
  it('creates a UUID v4 once and keeps it', async () => {
    const db = await freshDb()
    const first = await getOrCreateDeviceId(db)
    expect(first).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/)
    expect(await getOrCreateDeviceId(db)).toBe(first)
    expect(await db.get('config', 'device_id')).toBe(first)
  })
})

describe('requestPersistence', () => {
  it('reports what the browser answered', async () => {
    expect(await requestPersistence({ persist: vi.fn().mockResolvedValue(true) })).toBe(true)
    expect(await requestPersistence({ persist: vi.fn().mockResolvedValue(false) })).toBe(false)
  })
  it('treats a missing or failing API as not persisted', async () => {
    expect(await requestPersistence(undefined)).toBe(false)
    expect(await requestPersistence({ persist: vi.fn().mockRejectedValue(new Error('denied')) })).toBe(false)
  })
})
