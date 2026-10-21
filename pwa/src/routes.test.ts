import { expect, it } from 'vitest'
import { resolveRoute } from './routes'

it('maps the fixed paths and sends anything else to not_found', () => {
  expect(resolveRoute('/')).toEqual({ name: 'home' })
  expect(resolveRoute('/login')).toEqual({ name: 'login' })
  expect(resolveRoute('/return')).toEqual({ name: 'return' })
  expect(resolveRoute('/switch-operator')).toEqual({ name: 'switch_operator' })
  expect(resolveRoute('/profile')).toEqual({ name: 'profile' })
  expect(resolveRoute('/shelters/new')).toEqual({ name: 'shelter_new' })
  expect(resolveRoute('/inbox')).toEqual({ name: 'inbox' })
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
