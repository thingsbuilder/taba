import { expect, it } from 'vitest'
import { MEDIATOR_ROLES, canManageRoles, roleErrorKey } from './roles'
import en from './i18n/en.json'
import ptBR from './i18n/pt-BR.json'

it('only a validator or the Civil Defense manage roles', () => {
  expect(canManageRoles(['validator'])).toBe(true)
  expect(canManageRoles(['civil_defense'])).toBe(true)
  expect(canManageRoles([])).toBe(false)
  expect(canManageRoles(['mediator', 'protection'])).toBe(false)
})

it('maps 403 and 404 to their own texts and leaves the rest to the shared codes', () => {
  expect(roleErrorKey(403)).toBe('role.error_forbidden')
  expect(roleErrorKey(404)).toBe('role.error_not_found')
  expect(roleErrorKey(500)).toBeNull()
})

it('has pt-BR and en text for both roles and the messages', () => {
  expect(MEDIATOR_ROLES).toEqual(['mediator', 'protection'])
  for (const k of [...MEDIATOR_ROLES.map((r) => `role.${r}`), 'role.error_forbidden', 'role.error_not_found', 'role.granted', 'role.revoked']) {
    expect(ptBR, k).toHaveProperty([k])
    expect(en, k).toHaveProperty([k])
  }
})
