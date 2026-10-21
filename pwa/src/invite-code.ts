// An invite code is 128 random bits in base64url: 22 characters (server: invites.rs).
const CODE = /^[A-Za-z0-9_-]{22}$/

/** The code from a pasted invite URL (with or without query), a `/invite/<code>` path, or the bare code. */
export function extractInviteCode(input: string): string | null {
  const text = input.trim()
  if (CODE.test(text)) return text
  const path = text.split(/[?#]/)[0]!.replace(/\/+$/, '')
  const match = /\/invite\/([^/]+)$/.exec(path)
  return match && CODE.test(match[1]!) ? match[1]! : null
}
