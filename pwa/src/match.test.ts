import { describe, expect, it } from 'vitest'
import type { MatchSummary } from './api-phase4'
import { canDecide, decisionRequest, openMatches, queueKey, whatsappHref } from './match'
import en from './i18n/en.json'
import ptBR from './i18n/pt-BR.json'

const summary = (id: string, status: MatchSummary['status']): MatchSummary => ({
  id, status, mediator_kind: 'shelter', is_child: false, missing_person_name: 'Ana', created_at: '',
})

it('the inbox keeps only the open matches', () => {
  const list = [summary('a', 'open'), summary('b', 'accepted'), summary('c', 'declined'), summary('d', 'not_the_person'), summary('e', 'open')]
  expect(openMatches(list).map((m) => m.id)).toEqual(['a', 'e'])
})

describe('the check comes before any decision', () => {
  it('no decision without the check, and none while sending', () => {
    expect(canDecide(false, false)).toBe(false)
    expect(canDecide(true, true)).toBe(false)
    expect(canDecide(true, false)).toBe(true)
  })
})

describe('decisionRequest', () => {
  it('sends the decision, who decided and a trimmed note', () => {
    expect(decisionRequest('accept', 'operator', '  pessoa sedada  ')).toEqual({ decision: 'accept', decided_by: 'operator', note: 'pessoa sedada' })
  })
  it('leaves the note out when it is empty', () => {
    expect(decisionRequest('not_the_person', 'person', '  ')).toEqual({ decision: 'not_the_person', decided_by: 'person' })
  })
})

describe('whatsappHref', () => {
  it('opens only a wa.me link', () => {
    expect(whatsappHref('https://wa.me/5511999990000?text=oi')).toBe('https://wa.me/5511999990000?text=oi')
    expect(whatsappHref('javascript:alert(1)')).toBeNull()
    expect(whatsappHref('https://evil.example/wa.me/')).toBeNull()
    expect(whatsappHref(undefined)).toBeNull()
  })
})

it('has text for every queue, decision and the warning to erase the number, in pt-BR and en', () => {
  const keys = [
    ...(['shelter', 'central', 'protection'] as const).map(queueKey),
    ...['accept', 'decline', 'not_the_person'].flatMap((d) => [`match.${d}`, `match.${d}_help`]),
    'match.erase_warning', 'match.checked', 'match.by_person', 'match.by_operator', 'match.closed',
  ]
  for (const k of keys) {
    expect(ptBR, k).toHaveProperty([k])
    expect(en, k).toHaveProperty([k])
  }
  expect(ptBR['match.erase_warning']).toBe('Depois de combinar o contato, apague este número do seu celular.')
})
