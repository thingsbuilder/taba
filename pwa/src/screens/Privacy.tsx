import { useState } from 'preact/hooks'
import { CONSENT_LANGS, type ConsentLang, defaultConsentLang, privacyView } from '../consent'
import { getLanguage } from '../i18n'
import { BackLink } from './BackLink'

/** `/privacy`: the full text (spec 10.1.3) in PT | EN | ES. Public, so it needs no session. */
export function Privacy() {
  const [lang, setLang] = useState<ConsentLang>(defaultConsentLang(getLanguage()))
  const view = privacyView(lang)
  return (
    <main class="screen">
      <BackLink />
      <div class="lang" role="group" aria-label="PT | EN | ES">
        {CONSENT_LANGS.map(({ id, label }) => (
          <button type="button" class="lang-option" aria-pressed={lang === id} onClick={() => setLang(id)}>{label}</button>
        ))}
      </div>
      <h1 class="title">{view.title}</h1>
      <p class="body">{view.intro}</p>
      {view.sections.map((s) => (
        <section class="card" key={s.title}>
          <h2 class="subtitle">{s.title}</h2>
          <p class="body">{s.body}</p>
        </section>
      ))}
    </main>
  )
}
