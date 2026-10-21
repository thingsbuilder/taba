import { t } from '../i18n'

/** `/f/:code`, public (spec 3.7 item 11): the family sends the search. No login, no PIN, no top bar. Stage 0 stub; owner H. */
export function Family({ code: _code }: { code: string }) {
  return (
    <main class="screen">
      <h1 class="title">{t('family.title')}</h1>
    </main>
  )
}
