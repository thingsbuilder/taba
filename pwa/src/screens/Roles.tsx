import type { AppContext } from '../app-context'
import { t } from '../i18n'
import { BackLink } from './BackLink'

/** `/roles` (spec 2.1): a validator or the Civil Defense grants `mediator` and `protection`. Stage 0 stub; owner G. */
export function Roles({ ctx: _ctx }: { ctx: AppContext }) {
  return (
    <main class="screen">
      <BackLink />
      <h1 class="title">{t('role.title')}</h1>
    </main>
  )
}
