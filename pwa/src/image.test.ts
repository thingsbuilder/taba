import { describe, expect, it } from 'vitest'
import { MAX_BYTES, encodeUnderLimit, fitSize, squareCrop, toBase64 } from './image'

describe('fitSize', () => {
  it('limits the largest side to 1280 and keeps the proportion', () => {
    expect(fitSize(4000, 3000)).toEqual({ width: 1280, height: 960 })
    expect(fitSize(3000, 4000)).toEqual({ width: 960, height: 1280 })
  })
  it('never enlarges', () => {
    expect(fitSize(640, 480)).toEqual({ width: 640, height: 480 })
  })
})

describe('squareCrop', () => {
  it('centers the square on the short side', () => {
    expect(squareCrop(640, 480)).toEqual({ sx: 80, sy: 0, side: 480 })
    expect(squareCrop(480, 640)).toEqual({ sx: 0, sy: 80, side: 480 })
  })
})

describe('encodeUnderLimit', () => {
  const blobOf = (size: number) => new Blob([new Uint8Array(size)])

  it('keeps 0.9 when the file already fits', async () => {
    const tried: number[] = []
    const blob = await encodeUnderLimit(async (q) => (tried.push(q), blobOf(1000)))
    expect(tried).toEqual([0.9])
    expect(blob?.size).toBe(1000)
  })

  it('drops the quality by 0.05 until the file is below 800 KB', async () => {
    const tried: number[] = []
    const kb: Record<number, number> = { 0.9: 900, 0.85: 850, 0.8: 700 }
    const blob = await encodeUnderLimit(async (q) => (tried.push(q), blobOf(kb[q]! * 1024)))
    expect(tried).toEqual([0.9, 0.85, 0.8])
    expect(blob!.size).toBeLessThan(MAX_BYTES)
  })

  it('treats exactly 800 KB as too big', async () => {
    const tried: number[] = []
    await encodeUnderLimit(async (q) => (tried.push(q), blobOf(q > 0.85 ? MAX_BYTES : 10)))
    expect(tried).toEqual([0.9, 0.85])
  })

  it('gives up with null when even the lowest quality is too big', async () => {
    const tried: number[] = []
    const blob = await encodeUnderLimit(async (q) => (tried.push(q), blobOf(MAX_BYTES + 1)))
    expect(blob).toBeNull()
    expect(tried[0]).toBe(0.9)
    expect(tried.at(-1)).toBe(0.1)
    expect(tried).toHaveLength(17)
  })
})

describe('toBase64', () => {
  it('is the plain base64 of the bytes', async () => {
    expect(await toBase64(new Blob([new Uint8Array([104, 105])]))).toBe('aGk=')
  })
  it('handles files larger than one chunk', async () => {
    const size = 0x8000 * 3 + 5
    const out = await toBase64(new Blob([new Uint8Array(size).fill(1)]))
    expect(atob(out).length).toBe(size)
  })
})
