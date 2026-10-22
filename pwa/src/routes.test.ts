import { expect, it } from 'vitest'
import { resolveRoute, usesShell } from './routes'

it('maps the fixed paths and sends anything else to not_found', () => {
  expect(resolveRoute('/')).toEqual({ name: 'home' })
  expect(resolveRoute('/login')).toEqual({ name: 'login' })
  expect(resolveRoute('/return')).toEqual({ name: 'return' })
  expect(resolveRoute('/switch-operator')).toEqual({ name: 'switch_operator' })
  expect(resolveRoute('/profile')).toEqual({ name: 'profile' })
  expect(resolveRoute('/shelters/new')).toEqual({ name: 'shelter_new' })
  expect(resolveRoute('/inbox')).toEqual({ name: 'inbox' })
  expect(resolveRoute('/diagnostics')).toEqual({ name: 'diagnostics' })
  expect(resolveRoute('/rescue')).toEqual({ name: 'rescue' })
  expect(resolveRoute('/search')).toEqual({ name: 'search' })
  expect(resolveRoute('/roles')).toEqual({ name: 'roles' })
  expect(resolveRoute('/dashboard')).toEqual({ name: 'dashboard' })
  for (const bad of ['/shelter', '/shelter/', '/shelters', '/shelter/a/b', '/invite', '/invite/a/b', '/x']) {
    expect(resolveRoute(bad)).toEqual({ name: 'not_found' })
  }
})

it('reads the parameters of the shelter, invite and endorse paths', () => {
  expect(resolveRoute('/shelter/abc')).toEqual({ name: 'shelter', id: 'abc' })
  expect(resolveRoute('/shelter/abc/members')).toEqual({ name: 'shelter_members', id: 'abc' })
  expect(resolveRoute('/invite/Xy-_9')).toEqual({ name: 'invite', code: 'Xy-_9' })
  expect(resolveRoute('/endorse/Qq')).toEqual({ name: 'endorse', code: 'Qq' })
})

it('reads the phase 3 paths', () => {
  expect(resolveRoute('/shelter/s1/checkin')).toEqual({ name: 'shelter_checkin', id: 's1' })
  expect(resolveRoute('/shelter/s1/people')).toEqual({ name: 'shelter_people', id: 's1' })
  expect(resolveRoute('/shelter/s1/exit')).toEqual({ name: 'shelter_exit', id: 's1' })
  expect(resolveRoute('/shelter/s1/households/h1')).toEqual({ name: 'shelter_household', id: 's1', household_id: 'h1' })
  expect(resolveRoute('/person/p1')).toEqual({ name: 'person', id: 'p1' })
  expect(resolveRoute('/privacy')).toEqual({ name: 'privacy' })
  for (const bad of ['/shelter/s1/checkin/x', '/shelter/s1/households', '/shelter/s1/households/h1/x', '/person', '/person/a/b', '/privacy/x']) {
    expect(resolveRoute(bad), bad).toEqual({ name: 'not_found' })
  }
})

it('reads the public family page and the match check', () => {
  expect(resolveRoute('/f/Xy-_9')).toEqual({ name: 'family', code: 'Xy-_9' })
  expect(resolveRoute('/matches/abc')).toEqual({ name: 'match', id: 'abc' })
  for (const bad of ['/f', '/f/', '/f/a/b', '/matches', '/matches/a/b']) expect(resolveRoute(bad)).toEqual({ name: 'not_found' })
})

it('shows the family page outside the shell and every other screen inside it', () => {
  expect(usesShell(resolveRoute('/f/abc'))).toBe(false)
  for (const path of ['/', '/login', '/profile', '/search', '/rescue', '/inbox', '/matches/m1', '/roles', '/diagnostics', '/shelter/s1', '/nope']) {
    expect(usesShell(resolveRoute(path)), path).toBe(true)
  }
})

it('reads the phase 5 shelter screens', () => {
  expect(resolveRoute('/shelter/s1/delivery')).toEqual({ name: 'shelter_delivery', id: 's1' })
  expect(resolveRoute('/shelter/s1/items')).toEqual({ name: 'shelter_items', id: 's1' })
  expect(resolveRoute('/shelter/s1/review')).toEqual({ name: 'shelter_review', id: 's1' })
  expect(resolveRoute('/shelter/s1/nope')).toEqual({ name: 'not_found' })
})
