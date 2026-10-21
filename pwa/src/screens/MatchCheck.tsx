import type { AppContext } from '../app-context'
import { t } from '../i18n'
import { BackLink } from './BackLink'

/** `/matches/:id` (spec 4.8): the check with the person, then the decision. Stage 0 stub; owner G. */
export function MatchCheck({ ctx: _ctx, matchId: _matchId }: { ctx: AppContext; matchId: string }) {
  return (
    <main class="screen">
      <BackLink />
      <h1 class="title">{t('match.title')}</h1>
    </main>
  )
}
