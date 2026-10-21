import { describe, expect, it, vi } from 'vitest'
import { MIN_FRAME_SIDE, cameraErrorKey, cameraSupported, frameProblem, openCamera, stopStream, videoReady, waitForFirstFrame } from './camera'

describe('camera', () => {
  it('asks for the front or back camera without audio', async () => {
    const getUserMedia = vi.fn().mockResolvedValue({})
    await openCamera('environment', { mediaDevices: { getUserMedia } })
    expect(getUserMedia).toHaveBeenCalledWith({ video: { facingMode: 'environment', width: { ideal: 1920 }, height: { ideal: 1080 } }, audio: false })
  })

  it('is unsupported without getUserMedia and rejects', async () => {
    expect(cameraSupported({})).toBe(false)
    await expect(openCamera('user', {})).rejects.toMatchObject({ name: 'NotSupportedError' })
  })

  it.each([
    ['NotAllowedError', 'checkin.camera_denied'],
    ['NotReadableError', 'checkin.camera_busy'],
    ['NotFoundError', 'checkin.camera_unsupported'],
  ])('%s -> %s', (name, key) => {
    expect(cameraErrorKey({ name })).toBe(key)
  })

  it('stops every track', () => {
    const stop = vi.fn()
    stopStream({ getTracks: () => [{ stop }, { stop }] } as unknown as MediaStream)
    expect(stop).toHaveBeenCalledTimes(2)
    expect(() => stopStream(null)).not.toThrow()
  })
})


/** RGBA of `n` pixels, each from `value(i)`. */
const pixels = (n: number, value: (i: number) => number) => {
  const data = new Uint8ClampedArray(n * 4)
  for (let i = 0; i < n; i++) data.set([value(i), value(i), value(i), 255], i * 4)
  return data
}
const size = { width: 1280, height: 720 }
const scene = pixels(1024, (i) => (i % 2 === 0 ? 60 : 200)) // a picture with something in it

describe('frameProblem', () => {
  it('accepts a frame with a size and a picture', () => {
    expect(frameProblem(size, scene)).toBeNull()
    expect(frameProblem(size)).toBeNull() // size only
    expect(frameProblem({ width: MIN_FRAME_SIDE, height: 100 }, scene)).toBeNull()
  })

  it('refuses a frame with no size', () => {
    for (const bad of [{ width: 0, height: 0 }, { width: 1280, height: 0 }, { width: 0, height: 720 }, { width: NaN, height: 720 }]) {
      expect(frameProblem(bad, scene), JSON.stringify(bad)).toBe('empty')
    }
  })

  it('refuses a frame too small to matter', () => {
    expect(frameProblem({ width: 319, height: 240 }, scene)).toBe('too_small')
    expect(frameProblem({ width: 100, height: 319 }, scene)).toBe('too_small')
  })

  it('refuses a black or flat picture', () => {
    expect(frameProblem(size, pixels(1024, () => 0))).toBe('blank')
    expect(frameProblem(size, pixels(1024, () => 5))).toBe('blank') // almost black
    expect(frameProblem(size, pixels(1024, () => 128))).toBe('blank') // one flat grey
    expect(frameProblem(size, pixels(1024, () => 255))).toBe('blank') // all white
    expect(frameProblem(size, pixels(1024, (i) => 128 + (i % 2)))).toBe('blank') // noise of one level
  })

  it('a dark but real picture is accepted', () => {
    expect(frameProblem(size, pixels(1024, (i) => 20 + (i % 40)))).toBeNull()
  })
})

describe('waiting for the first frame', () => {
  it('is ready only with enough data and a size', () => {
    expect(videoReady({ readyState: 4, videoWidth: 1280, videoHeight: 720 })).toBe(true)
    expect(videoReady({ readyState: 1, videoWidth: 1280, videoHeight: 720 })).toBe(false)
    expect(videoReady({ readyState: 4, videoWidth: 0, videoHeight: 0 })).toBe(false)
  })

  it('waits until the picture comes, as a rear camera does', async () => {
    const video = { readyState: 0, videoWidth: 0, videoHeight: 0 }
    let slept = 0
    const sleep = async () => {
      slept += 1
      if (slept === 3) Object.assign(video, { readyState: 4, videoWidth: 1920, videoHeight: 1080 })
    }
    expect(await waitForFirstFrame(video, 5000, sleep)).toBe(true)
    expect(slept).toBe(3)
  })

  it('gives up when it never comes', async () => {
    const video = { readyState: 4, videoWidth: 0, videoHeight: 0 }
    expect(await waitForFirstFrame(video, 500, async () => {})).toBe(false)
  })
})
