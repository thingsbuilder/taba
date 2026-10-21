import { expect, it } from 'vitest'
import { resolveRoute } from './routes'

it('maps / to home and anything else to not_found', () => {
  expect(resolveRoute('/')).toBe('home')
  expect(resolveRoute('/shelter/abc')).toBe('not_found')
})
