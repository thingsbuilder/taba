// Card codes: 8 characters of Crockford base32, made on the phone (spec 3.5.1).

const ALPHABET = '0123456789ABCDEFGHJKMNPQRSTVWXYZ'
const CODE = /^[0-9A-HJKMNP-TV-Z]{8}$/

export function generateCardCode(): string {
  // 256 is a multiple of 32, so `& 31` has no bias.
  return Array.from(crypto.getRandomValues(new Uint8Array(8)), (b) => ALPHABET[b & 31]).join('')
}

/** What a person types or a QR holds, as a code; null when it is not one. Crockford: O is 0, I and L are 1. */
export function parseCardCode(text: string): string | null {
  const last = text.trim().split(/[/?#]/).filter(Boolean).pop() ?? ''
  const code = last.toUpperCase().replace(/[\s-]/g, '').replace(/O/g, '0').replace(/[IL]/g, '1')
  return CODE.test(code) ? code : null
}

/** "ABCD 1234": easier to read aloud and to write on a wristband. */
export const formatCardCode = (code: string): string => `${code.slice(0, 4)} ${code.slice(4)}`
