import type { AppContext } from '../app-context'
import { t } from '../i18n'
import { BackLink } from './BackLink'

/** `/rescue` (spec 3.6): register a rescued person at the landing point. Stage 0 stub; owner F. */
export function Rescue({ ctx: _ctx }: { ctx: AppContext }) {
  return (
    <main class="screen">
      <BackLink />
      <h1 class="title">{t('rescue.title')}</h1>
    </main>
  )
}
