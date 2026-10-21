import { expect, it } from 'vitest'
import { formatCardCode, generateCardCode, parseCardCode } from './card-code'

it('generates 8 Crockford characters, without I, L, O or U', () => {
  for (let i = 0; i < 200; i++) expect(generateCardCode()).toMatch(/^[0-9A-HJKMNP-TV-Z]{8}$/)
})

it('does not repeat in practice', () => {
  expect(new Set(Array.from({ length: 500 }, generateCardCode)).size).toBe(500)
})

it('reads typed text the Crockford way', () => {
  expect(parseCardCode(' abcd-1234 ')).toBe('ABCD1234')
  expect(parseCardCode('0O1IL567')).toBe('00111567')
  expect(parseCardCode('https://taba.thingsbuilder.com/c/ABCD1234')).toBe('ABCD1234')
})

it('refuses what is not a code', () => {
  for (const bad of ['', 'ABC', 'ABCD12345', 'ABCDU234', 'ABCD 12!4']) expect(parseCardCode(bad)).toBeNull()
})

it('formats in two groups', () => {
  expect(formatCardCode('ABCD1234')).toBe('ABCD 1234')
})
