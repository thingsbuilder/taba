import { describe, expect, it, vi } from 'vitest'
import { cameraErrorKey, cameraSupported, openCamera, stopStream } from './camera'

describe('camera', () => {
  it('asks for the front or back camera without audio', async () => {
    const getUserMedia = vi.fn().mockResolvedValue({})
    await openCamera('environment', { mediaDevices: { getUserMedia } })
    expect(getUserMedia).toHaveBeenCalledWith({ video: { facingMode: 'environment' }, audio: false })
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
