import { useEffect, useState } from 'preact/hooks'
import type { MatchSummary } from '../api-phase4'
import type { AppContext } from '../app-context'
import { errorKey } from '../app-context'
import { t } from '../i18n'
import { openMatches, queueKey } from '../match'
import { navigate } from '../nav'

/** Open matches of the queues this volunteer attends, on top of the inbox (spec 4.8): a stopped queue is of no use. */
export function MatchQueue({ ctx, online }: { ctx: AppContext; online: boolean }) {
  const { active, api } = ctx
  const [matches, setMatches] = useState<MatchSummary[] | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    if (!active || !online) return
    void api.listMatches(active.secret).then((res) => {
      if (res.ok) setMatches(openMatches(res.data.matches))
      // A volunteer with no queue may be refused: nothing to show, nothing wrong.
      else if (res.status !== 403) setError(res.error)
    })
  }, [online, active?.volunteer_id])

  if (error) return <p class="notice notice-error" role="alert">{t(errorKey(error))}</p>
  if (!matches || matches.length === 0) return null
  return (
    <section class="card" aria-label={t('inbox.matches', { count: matches.length })}>
      <h2 class="subtitle">{t('inbox.matches', { count: matches.length })}</h2>
      <ul class="list">
        {matches.map((m) => (
          <li class="row" key={m.id}>
            <span class="row-text">
              <span class="body-strong">{m.missing_person_name}</span>
              <span class="caption">
                {t(queueKey(m.mediator_kind))}
                {m.is_child ? ` · ${t('inbox.match_child')}` : ''}
                {m.shelter_name ? ` · ${m.shelter_name}` : ''}
              </span>
            </span>
            <button type="button" class="button" onClick={() => navigate(`/matches/${m.id}`)}>{t('inbox.match_check')}</button>
          </li>
        ))}
      </ul>
    </section>
  )
}
