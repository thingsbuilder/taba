import type { Db } from '../db'
import en from './en.json'
import es from './es.json'
import ptBR from './pt-BR.json'

export type Language = 'pt-BR' | 'en'

// es.json is empty for now: Spanish only covers the check-in consent screen (spec 11.3).
const dictionaries: Record<string, Record<string, string>> = { 'pt-BR': ptBR, en, es }

/** First entry of `navigator.languages` that starts with `pt` gives pt-BR; anything else gives en (spec 11.1). */
export function detectLanguage(languages: readonly string[]): Language {
  return languages.some((l) => l.toLowerCase().startsWith('pt')) ? 'pt-BR' : 'en'
}

export async function loadLanguage(db: Db, languages: readonly string[]): Promise<Language> {
  const saved = await db.get('config', 'language')
  return saved === 'pt-BR' || saved === 'en' ? saved : detectLanguage(languages)
}

export async function saveLanguage(db: Db, language: Language): Promise<void> {
  await db.put('config', language, 'language')
}

let current: Language = 'pt-BR'
const listeners = new Set<() => void>()

export function getLanguage(): Language {
  return current
}

export function setLanguage(language: Language): void {
  current = language
  if (typeof document !== 'undefined') document.documentElement.lang = language
  listeners.forEach((l) => l())
}

export function onLanguageChange(listener: () => void): () => void {
  listeners.add(listener)
  return () => listeners.delete(listener)
}

export function t(key: string, params: Record<string, string | number> = {}): string {
  const text = dictionaries[current]?.[key] ?? dictionaries.en[key] ?? key
  return text.replace(/\{(\w+)\}/g, (_, name) => String(params[name] ?? `{${name}}`))
}
