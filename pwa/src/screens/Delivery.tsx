import type { AppContext } from '../app-context'
import { t } from '../i18n'
import { BackLink } from './BackLink'

/** `/shelter/:id/delivery` (spec 3.5.3): read the card, pick who takes the item, deliver. Owner C. Stage 0 stub. */
export function Delivery({ ctx: _ctx, shelterId: _shelterId }: { ctx: AppContext; shelterId: string }) {
  return (
    <main class="screen">
      <BackLink />
      <h1 class="title">{t('delivery.title')}</h1>
    </main>
  )
}
