import { expect, it } from 'vitest'
import en from './en.json'
import es from './es.json'
import ptBR from './pt-BR.json'

const keys = (d: Record<string, string>) => Object.keys(d).sort()

it('pt-BR and en have the same keys', () => {
  expect(keys(en)).toEqual(keys(ptBR))
})

it('es only has the consent screen keys (spec 11.3) and the family page keys (phase 4), besides the provisional note', () => {
  const extra = Object.keys(es).filter((k) => !k.startsWith('consent.') && !k.startsWith('family.') && !k.startsWith('__'))
  expect(extra).toEqual([])
  // The family page is public in PT, EN and ES (spec 3.7 item 11): every family key exists in all three.
  for (const key of Object.keys(ptBR).filter((k) => k.startsWith('family.'))) {
    expect(es, key).toHaveProperty([key])
    expect(en, key).toHaveProperty([key])
  }
  // Every consent key of the screen exists in the three languages.
  for (const k of Object.keys(es).filter((k) => k.startsWith('consent.'))) {
    expect(ptBR, k).toHaveProperty([k])
    expect(en, k).toHaveProperty([k])
  }
})

it('has one marker per section of phase 3, so owners add keys in separate places', () => {
  const markers = Object.keys(ptBR).filter((k) => /^__\w+__$/.test(k))
  expect(markers).toEqual(['__consent__', '__checkin__', '__person__', '__household__', '__card__', '__child__', '__exit__', '__degraded__', '__queue__', '__rescue__', '__search__', '__family__', '__match__', '__role__', '__inbox__', '__end__'])
  // Each section's keys sit right after its marker.
  const all = Object.keys(ptBR)
  for (const prefix of ['consent', 'checkin', 'person', 'household', 'card', 'child', 'exit', 'queue', 'rescue', 'search', 'family', 'match', 'role', 'inbox']) {
    const start = all.indexOf(`__${prefix}__`)
    const next = all.findIndex((k, i) => i > start && k.startsWith('__'))
    for (const k of all.slice(start + 1, next)) expect(k.startsWith(`${prefix}.`), `${k} in ${prefix}`).toBe(true)
  }
})
