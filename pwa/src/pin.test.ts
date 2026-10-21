import { afterEach, describe, expect, it, vi } from 'vitest'
import { PBKDF2_ITERATIONS, createPinRecord, isValidPin, verifyPin } from './pin'

afterEach(() => vi.restoreAllMocks())

describe('isValidPin', () => {
  it.each([['123456', true], ['000000', true], ['12345', false], ['1234567', false], ['12345a', false], ['', false], ['12 456', false]])(
    '%j -> %s',
    (pin, expected) => expect(isValidPin(pin)).toBe(expected),
  )
})

describe('PIN record', () => {
  it('uses PBKDF2-SHA256 with 200 000 iterations', async () => {
    const derive = vi.spyOn(crypto.subtle, 'deriveBits')
    await createPinRecord('123456')
    expect(PBKDF2_ITERATIONS).toBe(200_000)
    expect(derive).toHaveBeenCalledTimes(1)
    expect(derive.mock.calls[0][0]).toMatchObject({ name: 'PBKDF2', hash: 'SHA-256', iterations: 200_000 })
  })

  it('keeps only a hash and a salt, never the PIN', async () => {
    const record = await createPinRecord('123456')
    expect(record.pin_hash).toMatch(/^[0-9a-f]{64}$/)
    expect(record.pin_salt).toMatch(/^[0-9a-f]{32}$/)
    expect(JSON.stringify(record)).not.toContain('123456')
  })

  it('gives every record its own salt, so equal PINs hash differently', async () => {
    const [a, b] = await Promise.all([createPinRecord('123456'), createPinRecord('123456')])
    expect(a.pin_salt).not.toBe(b.pin_salt)
    expect(a.pin_hash).not.toBe(b.pin_hash)
  })

  it('verifies the right PIN and refuses any other', async () => {
    const record = await createPinRecord('123456')
    expect(await verifyPin('123456', record)).toBe(true)
    expect(await verifyPin('123457', record)).toBe(false)
    expect(await verifyPin('', record)).toBe(false)
  })

  it('matches a known PBKDF2-SHA256 answer (computed with Node crypto.pbkdf2Sync)', async () => {
    const salt = '01'.repeat(16)
    const expected = '81a4bfb1b39860947530673f73db2fcbe521963f03ffad7ab2182bad172cecf1'
    expect(await verifyPin('123456', { pin_salt: salt, pin_hash: expected })).toBe(true)
  })
})
