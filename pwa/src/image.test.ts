import { afterEach, describe, expect, it, vi } from 'vitest'
import { MAX_BYTES, encodeUnderLimit, fitSize, photoForToken, squareCrop, thumbnailOf, toBase64 } from './image'

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

// `photoForToken` and `thumbnailOf` with a canvas that records what it was asked to do (the real
// one needs a browser): the sizes, the crop, the JPEG type and the qualities tried.
describe('the canvas of the photo and of the thumbnail', () => {
  interface FakeCanvas {
    width: number
    height: number
    draws: number[][]
    qualities: number[]
    types: string[]
    bytesAt: (quality: number, canvas: FakeCanvas) => number
  }

  function stubCanvas(bytesAt: FakeCanvas['bytesAt']): FakeCanvas {
    const canvas: FakeCanvas = { width: 0, height: 0, draws: [], qualities: [], types: [], bytesAt }
    const element = {
      get width() { return canvas.width },
      set width(v: number) { canvas.width = v },
      get height() { return canvas.height },
      set height(v: number) { canvas.height = v },
      getContext: () => ({ drawImage: (_source: unknown, ...args: number[]) => canvas.draws.push(args) }),
      toBlob: (done: (b: Blob | null) => void, type: string, quality: number) => {
        canvas.types.push(type)
        canvas.qualities.push(quality)
        done(new Blob([new Uint8Array(canvas.bytesAt(quality, canvas))]))
      },
    }
    vi.stubGlobal('document', { createElement: () => element })
    return canvas
  }

  const frame = (width: number, height: number) => ({ source: {} as CanvasImageSource, width, height })
  afterEach(() => vi.unstubAllGlobals())

  it('the photo is drawn with the largest side at 1280 and kept under 800 KB, as JPEG', async () => {
    // Too big until the quality reaches 0.75.
    const canvas = stubCanvas((q) => (q > 0.76 ? 900 * 1024 : 500 * 1024))
    const blob = await photoForToken(frame(4000, 3000))
    expect([canvas.width, canvas.height]).toEqual([1280, 960])
    expect(canvas.draws).toEqual([[0, 0, 1280, 960]])
    expect(canvas.types.every((t) => t === 'image/jpeg')).toBe(true)
    expect(canvas.qualities).toEqual([0.9, 0.85, 0.8, 0.75])
    expect(blob!.size).toBeLessThan(MAX_BYTES)
  })

  it('a small photo is not enlarged, and one that cannot get under 800 KB is refused', async () => {
    const small = stubCanvas(() => 10)
    await photoForToken(frame(640, 480))
    expect([small.width, small.height]).toEqual([640, 480])
    const huge = stubCanvas(() => MAX_BYTES + 1)
    expect(await photoForToken(frame(4000, 3000))).toBeNull()
    expect(huge.qualities.at(-1)).toBe(0.1)
  })

  it('the thumbnail is the centered square, 128x128, JPEG at 0.6', async () => {
    const canvas = stubCanvas(() => 6 * 1024)
    const blob = await thumbnailOf(frame(1280, 720))
    expect([canvas.width, canvas.height]).toEqual([128, 128])
    // 720x720 from the middle of a 1280-wide frame.
    expect(canvas.draws).toEqual([[280, 0, 720, 720, 0, 0, 128, 128]])
    expect(canvas.types).toEqual(['image/jpeg'])
    expect(canvas.qualities).toEqual([0.6])
    expect(blob.size).toBe(6 * 1024)
  })
})
