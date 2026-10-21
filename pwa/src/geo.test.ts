import { describe, expect, it, vi } from 'vitest'
import { accuracyOk, getPosition } from './geo'

it('accepts up to 100 m and nothing above', () => {
  expect(accuracyOk(100)).toBe(true)
  expect(accuracyOk(12.5)).toBe(true)
  expect(accuracyOk(100.1)).toBe(false)
  expect(accuracyOk(101)).toBe(false)
})

describe('getPosition', () => {
  it('asks for a fresh high accuracy fix and returns lat, lon and accuracy', async () => {
    const getCurrentPosition = vi.fn((ok: PositionCallback, _fail?: PositionErrorCallback | null, _options?: PositionOptions) =>
      ok({ coords: { latitude: -30.03, longitude: -51.2, accuracy: 8 } } as GeolocationPosition))
    expect(await getPosition({ getCurrentPosition })).toEqual({ lat: -30.03, lon: -51.2, accuracy: 8 })
    expect(getCurrentPosition.mock.calls[0]![2]).toEqual({ enableHighAccuracy: true, timeout: 30_000, maximumAge: 0 })
  })

  it('rejects when the device refuses', async () => {
    const getCurrentPosition = (_ok: PositionCallback, fail?: PositionErrorCallback | null) =>
      fail?.({ code: 1 } as GeolocationPositionError)
    await expect(getPosition({ getCurrentPosition })).rejects.toMatchObject({ code: 1 })
  })
})
