import { useState } from 'preact/hooks'
import type { LegalBasis } from '../api-phase3'
import { CONSENT_LANGS, type ConsentLang, LEGAL_BASES, consentT, consentView, defaultConsentLang } from '../consent'
import { getLanguage, t } from '../i18n'
import { navigate } from '../nav'

export type ConsentChoice = 'selfie' | 'decline'

/** The consent screen (spec 3.4, 10.1.3, 11.3): read by the person, in the language they pick. The two buttons weigh the same. */
export function Consent({ unconfirmed, responsible, onChoose }: {
  unconfirmed: boolean
  responsible: string | null
  onChoose: (choice: ConsentChoice, basis: LegalBasis) => void
}) {
  const [lang, setLang] = useState<ConsentLang>(defaultConsentLang(getLanguage()))
  const [basis, setBasis] = useState<LegalBasis>('consent')
  const view = consentView(lang, { unconfirmed, responsible })
  return (
    <main class="screen">
      <div class="lang" role="group" aria-label={t('consent.lang_label')}>
        {CONSENT_LANGS.map(({ id, label: text }) => (
          <button type="button" class="lang-option" aria-pressed={lang === id} onClick={() => setLang(id)}>{text}</button>
        ))}
      </div>
      <section class="card" lang={lang}>
        <h1 class="title">{view.title}</h1>
        {view.unconfirmed && <p class="notice notice-warning" role="status">{view.unconfirmed}</p>}
        {view.lines.map((line) => <p class="body" key={line}>{line}</p>)}
        {view.responsible && <p class="caption">{view.responsible}</p>}
        <details>
          <summary class="body-strong">{consentT(lang, 'consent.read_aloud')}</summary>
          <p class="body-large" lang={lang}>{view.short}</p>
        </details>
        <a class="back-link" href="/privacy" onClick={(e) => { e.preventDefault(); navigate('/privacy') }}>{consentT(lang, 'consent.full_text')}</a>
      </section>

      <fieldset class="card fieldset">
        <legend class="body-strong">{t('consent.basis_title')}</legend>
        {LEGAL_BASES.map((b) => (
          <label class="check" key={b}>
            <input type="radio" name="legal_basis" checked={basis === b} onChange={() => setBasis(b)} />
            {t(`consent.basis_${b}`)}
          </label>
        ))}
      </fieldset>

      <div class="choice">
        <button type="button" class="button" lang={lang} onClick={() => onChoose('selfie', basis)}>{view.selfie}</button>
        <button type="button" class="button" lang={lang} onClick={() => onChoose('decline', basis)}>{view.decline}</button>
      </div>
    </main>
  )
}
