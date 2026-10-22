import type { AppContext } from '../app-context'
import { t } from '../i18n'
import { BackLink } from './BackLink'

/** `/shelter/:id/items` (spec 3.5.2): the admin sets what the shelter gives. Owner D. Stage 0 stub. */
export function Items({ ctx: _ctx, shelterId: _shelterId }: { ctx: AppContext; shelterId: string }) {
  return (
    <main class="screen">
      <BackLink />
      <h1 class="title">{t('items.title')}</h1>
    </main>
  )
}
