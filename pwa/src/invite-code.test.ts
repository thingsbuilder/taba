import { expect, it } from 'vitest'
import { extractInviteCode } from './invite-code'

const code = 'AbCdEfGhIjKlMnOpQr_-12'

it('reads the code from a full URL, with query or fragment, or bare', () => {
  expect(extractInviteCode(`https://taba.example.org/invite/${code}`)).toBe(code)
  expect(extractInviteCode(`  https://taba.example.org/invite/${code}/ `)).toBe(code)
  expect(extractInviteCode(`https://taba.example.org/invite/${code}?utm=x&a=b`)).toBe(code)
  expect(extractInviteCode(`https://taba.example.org/invite/${code}#top`)).toBe(code)
  expect(extractInviteCode(`/invite/${code}`)).toBe(code)
  expect(extractInviteCode(code)).toBe(code)
})

it('refuses anything else', () => {
  for (const bad of ['', '   ', 'hello', `${code}x`, code.slice(1), `https://taba.example.org/invite/short`, `https://taba.example.org/shelter/${code}`, `https://taba.example.org/invite/${code}/extra`, 'AbCdEfGhIjKlMnOpQr+/12', `invite/${code}!`]) {
    expect(extractInviteCode(bad), bad).toBeNull()
  }
})
