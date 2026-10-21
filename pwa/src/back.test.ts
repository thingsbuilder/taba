import { expect, it } from 'vitest'
import { backTarget } from './back'
import { resolveRoute } from './routes'

it('goes to the logical parent of each inner screen', () => {
  expect(backTarget(resolveRoute('/shelter/abc/members'))).toBe('/shelter/abc')
  for (const path of ['/shelter/abc', '/inbox', '/shelters/new', '/endorse/code', '/invite/code']) {
    expect(backTarget(resolveRoute(path)), path).toBe('/')
  }
})

it('falls back to the home screen for anything else', () => {
  for (const path of ['/', '/nope', '/shelter']) expect(backTarget(resolveRoute(path)), path).toBe('/')
})
