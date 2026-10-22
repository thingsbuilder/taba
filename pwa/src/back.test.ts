import { expect, it } from 'vitest'
import { backTarget } from './back'
import { resolveRoute } from './routes'

it('goes to the logical parent of each inner screen', () => {
  expect(backTarget(resolveRoute('/shelter/abc/members'))).toBe('/shelter/abc')
  for (const path of ['/shelter/abc', '/inbox', '/shelters/new', '/endorse/code', '/invite/code', '/profile', '/switch-operator']) {
    expect(backTarget(resolveRoute(path)), path).toBe('/')
  }
})

it('goes from the diagnostics screen back to the profile', () => {
  expect(backTarget(resolveRoute('/diagnostics'))).toBe('/profile')
})

it('goes from the match check back to the inbox, and from the other phase 4 screens to the home screen', () => {
  expect(backTarget(resolveRoute('/matches/m1'))).toBe('/inbox')
  expect(backTarget(resolveRoute('/roles'))).toBe('/profile')
  for (const path of ['/rescue', '/search']) expect(backTarget(resolveRoute(path)), path).toBe('/')
})

it('falls back to the home screen for anything else', () => {
  for (const path of ['/', '/nope', '/shelter']) expect(backTarget(resolveRoute(path)), path).toBe('/')
})

it('goes from the phase 3 shelter screens back to the shelter, and from a person to the home screen', () => {
  for (const sub of ['checkin', 'people', 'exit', 'households/h1']) {
    expect(backTarget(resolveRoute(`/shelter/s1/${sub}`)), sub).toBe('/shelter/s1')
  }
  expect(backTarget(resolveRoute('/person/p1'))).toBe('/')
  expect(backTarget(resolveRoute('/privacy'))).toBe('/')
})
