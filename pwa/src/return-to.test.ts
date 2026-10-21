import { expect, it } from 'vitest'
import { setReturnTo, takeReturnTo } from './return-to'
import { freshDb } from './testing'

it('returns the saved path once', async () => {
  const db = await freshDb()
  await setReturnTo(db, '/endorse/abc')
  expect(await takeReturnTo(db)).toBe('/endorse/abc')
  expect(await takeReturnTo(db)).toBeNull()
})

it('refuses anything that is not an in-app path', async () => {
  const db = await freshDb()
  for (const bad of ['https://evil.test', '//evil.test', 'endorse/abc']) {
    await setReturnTo(db, bad)
    expect(await takeReturnTo(db)).toBeNull()
  }
})
