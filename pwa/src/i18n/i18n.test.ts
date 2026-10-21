import { describe, expect, it } from 'vitest'
import { freshDb } from '../testing'
import { detectLanguage, getLanguage, loadLanguage, saveLanguage, setLanguage, t } from './index'

describe('detectLanguage', () => {
  it.each([
    [['pt-BR'], 'pt-BR'],
    [['pt-PT'], 'pt-BR'],
    [['pt'], 'pt-BR'],
    [['en-US'], 'en'],
    [['es'], 'en'],
    [['fr'], 'en'],
    [['fr-FR', 'pt-BR'], 'pt-BR'],
    [['PT-br'], 'pt-BR'],
    [[], 'en'],
  ])('%j -> %s', (languages, expected) => {
    expect(detectLanguage(languages)).toBe(expected)
  })
})

describe('language persistence', () => {
  it('falls back to detection when nothing was saved', async () => {
    const db = await freshDb()
    expect(await loadLanguage(db, ['pt-PT'])).toBe('pt-BR')
    expect(await loadLanguage(db, ['fr'])).toBe('en')
  })

  it('the saved choice wins over the device language', async () => {
    const db = await freshDb()
    await saveLanguage(db, 'en')
    expect(await db.get('config', 'language')).toBe('en')
    expect(await loadLanguage(db, ['pt-BR'])).toBe('en')
  })

  it('ignores a saved value that is not a supported language', async () => {
    const db = await freshDb()
    await db.put('config', 'fr', 'language')
    expect(await loadLanguage(db, ['pt-BR'])).toBe('pt-BR')
  })
})

describe('t', () => {
  it('translates, interpolates and follows setLanguage', () => {
    setLanguage('pt-BR')
    expect(t('network.queue', { n: 3 })).toBe('3 na fila')
    setLanguage('en')
    expect(t('network.queue', { n: 3 })).toBe('3 queued')
    expect(getLanguage()).toBe('en')
  })

  it('returns the key when it is missing everywhere', () => {
    expect(t('does.not.exist')).toBe('does.not.exist')
  })
})
