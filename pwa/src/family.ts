import type { FamilyLanguage, SearchLinkStatus, SearchLinkSubmitRequest, SearchResponse } from './api-phase4'
import { type ConsentLang, consentT } from './consent'

// The family's public page (spec 3.7 item 11, 10.1.3). No session, no queue, no Diagnostics: the link code is a
// secret and the form is personal data, so these two calls use `fetch` directly instead of `createApi`, which logs routes.

export const MAX_PHOTOS = 5

export const FAMILY_LANGS: { id: FamilyLanguage; label: string }[] = [
  { id: 'pt-BR', label: 'PT' },
  { id: 'en', label: 'EN' },
  { id: 'es', label: 'ES' },
]

/** The page starts on the browser language: pt gives pt-BR, es gives es, anything else en. */
export function defaultFamilyLang(languages: readonly string[]): ConsentLang {
  for (const l of languages.map((x) => x.toLowerCase())) {
    if (l.startsWith('pt')) return 'pt-BR'
    if (l.startsWith('es')) return 'es'
    if (l.startsWith('en')) return 'en'
  }
  return 'en'
}

export const ft = (lang: ConsentLang, key: string, params?: Record<string, string | number>) => consentT(lang, `family.${key}`, params)

export interface FamilyForm {
  missing_name: string
  missing_nickname: string
  missing_notes: string
  family_name: string
  family_phone: string
  family_relationship: string
  family_language: FamilyLanguage
}

export const emptyForm = (lang: FamilyLanguage): FamilyForm => ({
  missing_name: '', missing_nickname: '', missing_notes: '', family_name: '', family_phone: '', family_relationship: '', family_language: lang,
})

export type FormField = 'missing_name' | 'family_name' | 'family_phone' | 'family_relationship'
export type FormProblem = FormField | 'photos' | 'consent'

const REQUIRED: FormField[] = ['missing_name', 'family_name', 'family_phone', 'family_relationship']

/** What stops the send: empty required fields, 0 or more than 5 photos, consent not ticked. */
export function validate(form: FamilyForm, photoCount: number, consented: boolean): FormProblem[] {
  const problems: FormProblem[] = REQUIRED.filter((k) => form[k].trim() === '')
  if (photoCount < 1 || photoCount > MAX_PHOTOS) problems.push('photos')
  if (!consented) problems.push('consent')
  return problems
}

export function buildRequest(form: FamilyForm, photos: string[]): SearchLinkSubmitRequest {
  const nickname = form.missing_nickname.trim()
  return {
    photos_base64: photos,
    missing_person: { name: form.missing_name.trim(), ...(nickname ? { nickname } : {}), notes: form.missing_notes.trim() },
    family: {
      name: form.family_name.trim(),
      phone: form.family_phone.trim(),
      relationship: form.family_relationship.trim(),
      language: form.family_language,
    },
  }
}

type Fetch = typeof fetch

async function publicCall<T>(fetchFn: Fetch, method: string, path: string, body?: unknown): Promise<{ ok: true; data: T } | { ok: false; status: number; error: string }> {
  let res: Response
  try {
    res = await fetchFn(path, {
      method,
      headers: body === undefined ? undefined : { 'Content-Type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body),
      cache: 'no-store',
    })
  } catch {
    return { ok: false, status: 0, error: 'network' }
  }
  const json = await res.json().catch(() => null)
  if (res.ok) return { ok: true, data: json as T }
  return { ok: false, status: res.status, error: typeof json?.error === 'string' ? json.error : 'error' }
}

export type LinkState = 'valid' | 'invalid' | 'network'

/** `GET /v1/search-links/{code}`: 410 and `valid: false` both mean the link is gone; the page says nothing else. */
export async function checkLink(code: string, fetchFn: Fetch = fetch): Promise<LinkState> {
  const res = await publicCall<SearchLinkStatus>(fetchFn, 'GET', `/v1/search-links/${encodeURIComponent(code)}`)
  if (res.ok) return res.data.valid ? 'valid' : 'invalid'
  return res.status === 410 || res.status === 404 ? 'invalid' : 'network'
}

const CONTENTION_RETRIES = 3
const CONTENTION_WAIT_MS = 1000

export type SubmitResult =
  | { kind: 'done'; usable: number }
  | { kind: 'invalid' }
  | { kind: 'error'; key: 'error_offline' | 'error_degraded' | 'error_too_big' | 'error_busy' | 'error_contention' | 'error_check' | 'error_generic' }

/** `POST /v1/search-links/{code}/submit`. The answer of a success is the same with or without a match; only `usable_photos` is read. */
export async function submitSearch(
  code: string,
  request: SearchLinkSubmitRequest,
  fetchFn: Fetch = fetch,
  sleep: (ms: number) => Promise<void> = (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
): Promise<SubmitResult> {
  let res = await publicCall<SearchResponse>(fetchFn, 'POST', `/v1/search-links/${encodeURIComponent(code)}/submit`, request)
  // `contention`: the server was busy with this same link; it asks to try again at once, so the page does, a few times.
  for (let retry = 0; retry < CONTENTION_RETRIES && !res.ok && res.status === 503 && res.error === 'contention'; retry++) {
    await sleep(CONTENTION_WAIT_MS)
    res = await publicCall<SearchResponse>(fetchFn, 'POST', `/v1/search-links/${encodeURIComponent(code)}/submit`, request)
  }
  if (res.ok) return { kind: 'done', usable: res.data.usable_photos }
  switch (res.status) {
    case 0: return { kind: 'error', key: 'error_offline' }
    case 410: return { kind: 'invalid' }
    case 413: return { kind: 'error', key: 'error_too_big' }
    case 429: return { kind: 'error', key: 'error_busy' }
    case 400: case 422: return { kind: 'error', key: 'error_check' }
    case 503: return { kind: 'error', key: res.error === 'unico_degraded' ? 'error_degraded' : res.error === 'contention' ? 'error_contention' : 'error_generic' }
    default: return { kind: 'error', key: 'error_generic' }
  }
}
