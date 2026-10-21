import { describe, expect, it } from 'vitest'
import en from './i18n/en.json'
import es from './i18n/es.json'
import ptBR from './i18n/pt-BR.json'
import { CONSENT_LANGS, LEGAL_BASES, effectiveBasis, consentT, consentView, defaultConsentLang, defaultLegalBasis, rememberResponsible, responsibleOf } from './consent'
import { detectLanguage, getLanguage, setLanguage, t } from './i18n'
import { freshDb } from './testing'

const SCREEN_KEYS = ['consent.title', 'consent.purpose', 'consent.retention', 'consent.access', 'consent.responsible', 'consent.unconfirmed', 'consent.no_police', 'consent.refusal_ok', 'consent.selfie', 'consent.decline', 'consent.short', 'consent.lang_label', 'consent.read_aloud', 'consent.full_text']

describe('the consent screen in three languages', () => {
  it.each([['pt-BR', ptBR], ['en', en], ['es', es]] as const)('%s has every text of the screen, none empty', (_lang, dict) => {
    for (const key of SCREEN_KEYS) expect((dict as Record<string, string>)[key], key).toMatch(/\S/)
  })

  it('offers PT | EN | ES', () => {
    expect(CONSENT_LANGS.map((l) => l.label)).toEqual(['PT', 'EN', 'ES'])
  })

  it.each([
    ['pt-BR', 'Recusar a selfie não tira abrigo nem doação.', 'Os dados não são compartilhados com polícia nem com imigração.'],
    ['en', 'Refusing the selfie does not take away shelter or donations.', 'The data is not shared with the police or with immigration.'],
    ['es', 'Rechazar la selfie no quita el albergue ni las donaciones.', 'Los datos no se comparten con la policía ni con inmigración.'],
  ] as const)('%s carries the two mandatory sentences', (lang, refusal, police) => {
    const view = consentView(lang, { unconfirmed: false, responsible: null })
    expect(view.lines).toContain(refusal)
    expect(view.lines).toContain(police)
  })

  it('shows the responsible and the unconfirmed notice when there are, and omits them when there are not', () => {
    const full = consentView('es', { unconfirmed: true, responsible: 'Ana Lima' })
    expect(full.responsible).toBe('Responsable del piloto: Ana Lima')
    expect(full.unconfirmed).toBe('Albergue no confirmado por la Defensa Civil')
    const bare = consentView('es', { unconfirmed: false, responsible: null })
    expect(bare.responsible).toBeNull()
    expect(bare.unconfirmed).toBeNull()
  })

  it('has the two buttons and the short version to read aloud', () => {
    const v = consentView('en', { unconfirmed: false, responsible: null })
    expect(v.selfie).toBe('Take the selfie')
    expect(v.decline).toBe('I prefer not to')
    expect(v.short.length).toBeGreaterThan(40)
  })
})

describe('language of the consent apart from the interface (criterion 58)', () => {
  it('starts on the interface language and ignores it afterwards', () => {
    expect(defaultConsentLang('pt-BR')).toBe('pt-BR')
    expect(defaultConsentLang('en')).toBe('en')
    setLanguage('pt-BR')
    expect(consentT('es', 'consent.selfie')).toBe('Hacer la selfie')
    expect(getLanguage()).toBe('pt-BR')
  })

  it('es -> English everywhere except the consent; pt-PT -> pt-BR', () => {
    expect(detectLanguage(['es-AR'])).toBe('en')
    expect(detectLanguage(['pt-PT'])).toBe('pt-BR')
    setLanguage('en')
    expect(t('checkin.title')).toBe(en['checkin.title'])
    expect(consentT('es', 'consent.title')).toBe('Antes de empezar')
    expect(Object.keys(es).filter((k) => !k.startsWith('consent.') && !k.startsWith('__'))).toEqual([])
  })

  it('a key Spanish lacks falls back to English, never to the volunteer language', () => {
    setLanguage('pt-BR')
    expect(consentT('es', 'checkin.title')).toBe(en['checkin.title'])
  })
})

describe('legal basis', () => {
  it('has the three of the spec and a child starts on child_protection', () => {
    expect(LEGAL_BASES).toEqual(['consent', 'vital_interest', 'child_protection'])
    expect(defaultLegalBasis(true)).toBe('child_protection')
    expect(defaultLegalBasis(false)).toBe('consent')
    expect(effectiveBasis('consent', true)).toBe('child_protection')
    expect(effectiveBasis('vital_interest', true)).toBe('vital_interest')
    expect(effectiveBasis('consent', false)).toBe('consent')
  })
})

describe('pilot responsible from GET /v1/status', () => {
  it('uses the one the server sent and remembers it for offline reading', async () => {
    const db = await freshDb()
    const status = { unico: 'ok' as const, pilot_responsible: 'Ana Lima' }
    await rememberResponsible(db, status)
    expect(await responsibleOf(db, status)).toBe('Ana Lima')
    expect(await responsibleOf(db, null)).toBe('Ana Lima')
  })
  it('is null, without failing, when the server does not send it yet', async () => {
    const db = await freshDb()
    await rememberResponsible(db, { unico: 'ok' })
    expect(await responsibleOf(db, { unico: 'ok' })).toBeNull()
    expect(await responsibleOf(db, null)).toBeNull()
  })
})
