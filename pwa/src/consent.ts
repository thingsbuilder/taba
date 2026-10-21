import type { Db } from './db'
import en from './i18n/en.json'
import es from './i18n/es.json'
import ptBR from './i18n/pt-BR.json'
import type { Language } from './i18n'
import type { ApiStatus } from './api'
import type { LegalBasis } from './api-phase3'

// The consent screen (spec 3.4, 10.1.3, 11.3). It is read by the person being registered, so it has its own
// PT | EN | ES selector, apart from the volunteer's interface language. Spanish exists only here.

export type ConsentLang = 'pt-BR' | 'en' | 'es'

export const CONSENT_LANGS: { id: ConsentLang; label: string }[] = [
  { id: 'pt-BR', label: 'PT' },
  { id: 'en', label: 'EN' },
  { id: 'es', label: 'ES' },
]

const dictionaries: Record<ConsentLang, Record<string, string>> = { 'pt-BR': ptBR, en, es }

/** A consent text in the chosen language; a key missing there falls back to English, never to the volunteer's language. */
export function consentT(lang: ConsentLang, key: string, params: Record<string, string | number> = {}): string {
  const text = dictionaries[lang][key] ?? dictionaries.en[key] ?? key
  return text.replace(/\{(\w+)\}/g, (_, name) => String(params[name] ?? `{${name}}`))
}

/** The selector starts on the interface language (spec 11.3). */
export const defaultConsentLang = (ui: Language): ConsentLang => ui

export interface ConsentView {
  title: string
  /** In reading order: purpose, retention, access, then the two mandatory sentences. */
  lines: string[]
  unconfirmed: string | null
  responsible: string | null
  short: string
  selfie: string
  decline: string
}

export function consentView(lang: ConsentLang, opts: { unconfirmed: boolean; responsible: string | null }): ConsentView {
  const t = (key: string, params?: Record<string, string | number>) => consentT(lang, key, params)
  return {
    title: t('consent.title'),
    lines: [t('consent.purpose'), t('consent.retention'), t('consent.access'), t('consent.refusal_ok'), t('consent.no_police')],
    unconfirmed: opts.unconfirmed ? t('consent.unconfirmed') : null,
    responsible: opts.responsible ? t('consent.responsible', { name: opts.responsible }) : null,
    short: t('consent.short'),
    selfie: t('consent.selfie'),
    decline: t('consent.decline'),
  }
}

export const LEGAL_BASES: LegalBasis[] = ['consent', 'vital_interest', 'child_protection']

/** The basis the volunteer starts with: a child is `child_protection` (spec 3.4.1), otherwise `consent`. */
export const defaultLegalBasis = (isChild: boolean): LegalBasis => (isChild ? 'child_protection' : 'consent')

/** A child is `child_protection` (spec 3.4.1) unless the volunteer chose `vital_interest` for an injured one. */
export const effectiveBasis = (basis: LegalBasis, isChild: boolean): LegalBasis => (isChild && basis === 'consent' ? 'child_protection' : basis)

// The responsible of the pilot comes from `GET /v1/status` (`pilot_responsible`). The consent has to be
// readable offline, so the last value seen stays on the phone; a server that does not send it yet gives null.

export async function rememberResponsible(db: Db, status: ApiStatus | null): Promise<void> {
  if (status?.pilot_responsible) await db.put('config', status.pilot_responsible, 'pilot_responsible')
}

export async function responsibleOf(db: Db, status: ApiStatus | null): Promise<string | null> {
  if (status?.pilot_responsible) return status.pilot_responsible
  const saved = await db.get('config', 'pilot_responsible')
  return typeof saved === 'string' && saved !== '' ? saved : null
}
