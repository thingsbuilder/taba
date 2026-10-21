import { expect, it } from 'vitest'
import { gateFor } from './install-gate'
import { resolveRoute } from './routes'

it('lets everything through once installed or off iOS', () => {
  for (const path of ['/', '/invite/abc', '/shelter/x']) expect(gateFor(resolveRoute(path), false)).toBe('open')
})

it('on an iPhone in Safari only the invite page escapes the install screen', () => {
  expect(gateFor(resolveRoute('/invite/AbCdEfGhIjKlMnOpQr_-12'), true)).toBe('invite_handoff')
  for (const path of ['/', '/login', '/return', '/shelters/new', '/shelter/x', '/shelter/x/members', '/endorse/abc', '/inbox', '/invite', '/nope']) {
    expect(gateFor(resolveRoute(path), true), path).toBe('install')
  }
})
