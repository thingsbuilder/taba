import { expect, it } from 'vitest'
import { resolveRoute } from './routes'

it('maps the phase 1 paths and sends anything else to not_found', () => {
  expect(resolveRoute('/')).toBe('home')
  expect(resolveRoute('/login')).toBe('login')
  expect(resolveRoute('/return')).toBe('return')
  expect(resolveRoute('/switch-operator')).toBe('switch_operator')
  expect(resolveRoute('/profile')).toBe('profile')
  expect(resolveRoute('/shelter/abc')).toBe('not_found')
})
