import type { AppContext } from '../app-context'
import { t } from '../i18n'
import { BackLink } from './BackLink'

/** `/dashboard` (spec 3.10): the Civil Defense panel, the daily CSV and the shelters to confirm. Stage 0 stub; owner F. */
export function Dashboard({ ctx: _ctx }: { ctx: AppContext }) {
  return (
    <main class="screen">
      <BackLink />
      <h1 class="title">{t('dashboard.title')}</h1>
    </main>
  )
}
