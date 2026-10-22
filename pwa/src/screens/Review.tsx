import type { AppContext } from '../app-context'
import { t } from '../i18n'
import { BackLink } from './BackLink'

/** `/shelter/:id/review` (spec 3.5.3 item 8): the admin reviews the deliveries in review. Owner D. Stage 0 stub. */
export function Review({ ctx: _ctx, shelterId: _shelterId }: { ctx: AppContext; shelterId: string }) {
  return (
    <main class="screen">
      <BackLink />
      <h1 class="title">{t('review.title')}</h1>
    </main>
  )
}
