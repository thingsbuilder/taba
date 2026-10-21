import { t } from '../i18n'
import { BackLink } from './BackLink'

const SECTIONS = [1, 2, 3, 4, 5, 6, 7, 8, 9]

/** `/privacy`: the full text (spec 10.1.3). Public, so it needs no session. */
export function Privacy() {
  return (
    <main class="screen">
      <BackLink />
      <h1 class="title">{t('consent.privacy_title')}</h1>
      <p class="body">{t('consent.privacy_intro')}</p>
      {SECTIONS.map((n) => (
        <section class="card" key={n}>
          <h2 class="subtitle">{t(`consent.privacy_s${n}_title`)}</h2>
          <p class="body">{t(`consent.privacy_s${n}_body`)}</p>
        </section>
      ))}
    </main>
  )
}
