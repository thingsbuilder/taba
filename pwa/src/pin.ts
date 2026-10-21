// Local PIN for switching operators on a shared device (spec 3.1.1). Only the
// PBKDF2-SHA256 hash and its salt are stored. The PIN guards against casual use of the
// device; it does not replace the server session.

export const PIN_LENGTH = 6
export const PBKDF2_ITERATIONS = 200_000
const SALT_BYTES = 16

export interface PinRecord {
  pin_salt: string // hex
  pin_hash: string // hex
}

export const isValidPin = (pin: string): boolean => /^\d{6}$/.test(pin)

const toHex = (bytes: Uint8Array): string => Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('')
const fromHex = (hex: string): Uint8Array => Uint8Array.from(hex.match(/../g) ?? [], (h) => parseInt(h, 16))

async function derive(pin: string, salt: Uint8Array): Promise<string> {
  const key = await crypto.subtle.importKey('raw', new TextEncoder().encode(pin), 'PBKDF2', false, ['deriveBits'])
  const bits = await crypto.subtle.deriveBits(
    { name: 'PBKDF2', hash: 'SHA-256', salt: salt as BufferSource, iterations: PBKDF2_ITERATIONS },
    key,
    256,
  )
  return toHex(new Uint8Array(bits))
}

/** A new random salt per PIN. */
export async function createPinRecord(pin: string): Promise<PinRecord> {
  const salt = crypto.getRandomValues(new Uint8Array(SALT_BYTES))
  return { pin_salt: toHex(salt), pin_hash: await derive(pin, salt) }
}

export async function verifyPin(pin: string, record: PinRecord): Promise<boolean> {
  return (await derive(pin, fromHex(record.pin_salt))) === record.pin_hash
}
