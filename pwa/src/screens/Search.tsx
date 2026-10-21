import type { AppContext } from '../app-context'
import { t } from '../i18n'
import { BackLink } from './BackLink'

/** `/search` (spec 3.7): register a search, or send the family a link. Stage 0 stub; owner F. */
export function Search({ ctx: _ctx }: { ctx: AppContext }) {
  return (
    <main class="screen">
      <BackLink />
      <h1 class="title">{t('search.title')}</h1>
    </main>
  )
}
