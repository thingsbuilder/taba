import { useEffect, useRef, useState } from 'preact/hooks'
import type { FamilyLanguage } from '../api-phase4'
import type { ConsentLang } from '../consent'
import { FAMILY_LANGS, type FamilyForm, type FormField, type FormProblem, type LinkState, MAX_PHOTOS, buildRequest, checkLink, defaultFamilyLang, emptyForm, ft, submitSearch, validate } from '../family'
import { photoForToken, toBase64 } from '../image'

interface Photo {
  blob: Blob
  url: string
}

type Outcome = { kind: 'done'; usable: number; sent: number } | null

/** `/f/:code`, public (spec 3.7 item 11): the family sends the search. No login, PIN, top bar or queue; nothing is logged. */
export function Family({ code }: { code: string }) {
  const [lang, setLang] = useState<ConsentLang>(() => defaultFamilyLang(navigator.languages ?? []))
  const [link, setLink] = useState<LinkState | null>(null)
  const [form, setForm] = useState<FamilyForm>(() => emptyForm(defaultFamilyLang(navigator.languages ?? []) as FamilyLanguage))
  const [langTouched, setLangTouched] = useState(false)
  const [photos, setPhotos] = useState<Photo[]>([])
  const [consented, setConsented] = useState(false)
  const [problems, setProblems] = useState<FormProblem[]>([])
  const [message, setMessage] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [outcome, setOutcome] = useState<Outcome>(null)
  const urls = useRef<string[]>([])

  const load = () => {
    setLink(null)
    void checkLink(code).then(setLink)
  }
  useEffect(load, [code])
  useEffect(() => {
    document.documentElement.lang = lang
  }, [lang])
  useEffect(() => () => urls.current.forEach((u) => URL.revokeObjectURL(u)), [])

  const t = (key: string, params?: Record<string, string | number>) => ft(lang, key, params)
  const pickLang = (id: ConsentLang) => {
    setLang(id)
    // The family's own language follows the page until they choose it on its own.
    if (!langTouched) setForm((f) => ({ ...f, family_language: id as FamilyLanguage }))
  }
  const set = <K extends keyof FamilyForm>(key: K, value: FamilyForm[K]) => setForm((f) => ({ ...f, [key]: value }))
  const bad = (p: FormProblem) => problems.includes(p)

  async function addFiles(files: FileList | null) {
    if (!files) return
    setMessage(null)
    const added: Photo[] = []
    for (const file of Array.from(files).slice(0, MAX_PHOTOS - photos.length)) {
      try {
        const bitmap = await createImageBitmap(file)
        const blob = await photoForToken({ source: bitmap, width: bitmap.width, height: bitmap.height })
        bitmap.close()
        if (!blob) throw new Error('too_big')
        const url = URL.createObjectURL(blob)
        urls.current.push(url)
        added.push({ blob, url })
      } catch {
        setMessage(t('photo_failed'))
      }
    }
    setPhotos((p) => [...p, ...added].slice(0, MAX_PHOTOS))
  }
  const removePhoto = (i: number) => {
    URL.revokeObjectURL(photos[i]!.url)
    setPhotos((p) => p.filter((_, j) => j !== i))
  }

  async function send(e: Event) {
    e.preventDefault()
    if (busy) return
    const found = validate(form, photos.length, consented)
    setProblems(found)
    if (found.length > 0) {
      setMessage(t(found.includes('consent') ? 'fix_consent' : found.includes('photos') && found.length === 1 ? 'fix_photos' : 'fix_form'))
      return
    }
    setBusy(true)
    setMessage(null)
    const result = await submitSearch(code, buildRequest(form, await Promise.all(photos.map((p) => toBase64(p.blob)))))
    setBusy(false)
    if (result.kind === 'done') {
      urls.current.forEach((u) => URL.revokeObjectURL(u))
      urls.current = []
      setOutcome({ kind: 'done', usable: result.usable, sent: photos.length })
      setPhotos([])
    } else if (result.kind === 'invalid') setLink('invalid')
    else setMessage(t(result.key))
  }

  const field = (key: FormField, label: string, extra: Record<string, string> = {}) => (
    <label class="field">
      {label}
      <input type="text" value={form[key]} aria-invalid={bad(key)} onInput={(e) => set(key, e.currentTarget.value)} {...extra} />
    </label>
  )

  return (
    <main class="screen" lang={lang}>
      <div class="lang" role="group" aria-label={t('lang_label')}>
        {FAMILY_LANGS.map(({ id, label }) => (
          <button type="button" class="lang-option" aria-pressed={lang === id} onClick={() => pickLang(id)}>{label}</button>
        ))}
      </div>
      <h1 class="title">{t('title')}</h1>
      <p class="notice notice-warning">{t('notice')}</p>

      {link === null && <p class="body" role="status">{t('loading')}</p>}
      {link === 'network' && (
        <>
          <p class="notice notice-error" role="alert">{t('error_offline')}</p>
          <button type="button" class="button" onClick={load}>{t('retry')}</button>
        </>
      )}
      {link === 'invalid' && <p class="notice notice-error" role="alert">{t('invalid')}</p>}

      {link === 'valid' && outcome && (
        <section class="card" role="status">
          <h2 class="subtitle">{t('done_title')}</h2>
          <p class="body">{t('done_message')}</p>
          {outcome.usable < outcome.sent && <p class="notice notice-warning">{t('done_fewer_photos', { usable: outcome.usable, sent: outcome.sent })}</p>}
        </section>
      )}

      {link === 'valid' && !outcome && (
        <form class="form" onSubmit={(e) => void send(e)} noValidate>
          <section class="card">
            <h2 class="subtitle">{t('consent_title')}</h2>
            {['consent_stored', 'consent_retention', 'consent_access', 'consent_basis', 'consent_no_police'].map((k) => <p class="body" key={k}>{t(k)}</p>)}
            <label class="check">
              <input type="checkbox" checked={consented} aria-invalid={bad('consent')} onChange={(e) => setConsented(e.currentTarget.checked)} />
              {t('consent_accept')}
            </label>
          </section>

          <section class="card form">
            <h2 class="subtitle">{t('missing_title')}</h2>
            {field('missing_name', t('missing_name'), { autocomplete: 'off' })}
            <label class="field">
              {t('missing_nickname')}
              <input type="text" value={form.missing_nickname} autocomplete="off" onInput={(e) => set('missing_nickname', e.currentTarget.value)} />
            </label>
            <label class="field">
              {t('missing_notes')}
              <textarea class="field-text" rows={3} value={form.missing_notes} onInput={(e) => set('missing_notes', e.currentTarget.value)} />
            </label>
          </section>

          <section class="card form">
            <h2 class="subtitle">{t('family_title')}</h2>
            {field('family_name', t('family_name'), { autocomplete: 'name' })}
            {field('family_phone', t('family_phone'), { type: 'tel', inputmode: 'tel', autocomplete: 'tel' })}
            {field('family_relationship', t('family_relationship'), { autocomplete: 'off' })}
            <label class="field">
              {t('family_language')}
              <select value={form.family_language} onChange={(e) => { setLangTouched(true); set('family_language', e.currentTarget.value as FamilyLanguage) }}>
                <option value="pt-BR">Português</option>
                <option value="en">English</option>
                <option value="es">Español</option>
              </select>
            </label>
          </section>

          <section class="card">
            <h2 class="subtitle">{t('photos_title')}</h2>
            <p class="body">{t('photos_hint')}</p>
            {photos.length > 0 && (
              <ul class="list">
                {photos.map((p, i) => (
                  <li class="card row" key={p.url}>
                    <img class="thumbnail" src={p.url} width={64} height={64} style="object-fit: cover" alt={t('photo_alt', { n: i + 1 })} />
                    <button type="button" class="button button-secondary" onClick={() => removePhoto(i)}>{t('photos_remove', { n: i + 1 })}</button>
                  </li>
                ))}
              </ul>
            )}
            {photos.length < MAX_PHOTOS && (
              <label class={`button button-secondary${bad('photos') ? ' notice-error' : ''}`}>
                {t('photos_add')} ({photos.length}/{MAX_PHOTOS})
                <input type="file" accept="image/*" multiple hidden onChange={(e) => { void addFiles(e.currentTarget.files); e.currentTarget.value = '' }} />
              </label>
            )}
          </section>

          {message && <p class="notice notice-error" role="alert">{message}</p>}
          <button type="submit" class="button" disabled={busy}>{busy ? t('sending') : t('send')}</button>
        </form>
      )}
    </main>
  )
}
