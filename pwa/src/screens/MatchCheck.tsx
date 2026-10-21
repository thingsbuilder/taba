import { useEffect, useState } from 'preact/hooks'
import type { DecidedBy, MatchDecision, MatchDecisionResponse } from '../api-phase4'
import type { MatchDetail } from '../api-phase4'
import type { AppContext } from '../app-context'
import { errorKey } from '../app-context'
import { logEvent } from '../diagnostics'
import { useOnline } from '../hooks'
import { t } from '../i18n'
import { canDecide, decisionRequest, queueKey, whatsappHref } from '../match'
import { navigate } from '../nav'
import { BackLink } from './BackLink'

const DECISIONS: { decision: MatchDecision; secondary: boolean }[] = [
  { decision: 'accept', secondary: false },
  { decision: 'decline', secondary: true },
  { decision: 'not_the_person', secondary: true },
]

/** `/matches/:id` (spec 4.8): the check with the person, then the decision. Needs signal. */
export function MatchCheck({ ctx, matchId }: { ctx: AppContext; matchId: string }) {
  const { active, api } = ctx
  const online = useOnline()
  const [match, setMatch] = useState<MatchDetail | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [checked, setChecked] = useState(false)
  const [decidedBy, setDecidedBy] = useState<DecidedBy>('person')
  const [note, setNote] = useState('')
  const [busy, setBusy] = useState(false)
  const [closed, setClosed] = useState(false)
  const [done, setDone] = useState<{ decision: MatchDecision; response: MatchDecisionResponse } | null>(null)

  useEffect(() => {
    if (!active) return navigate('/login', true)
    if (!online) return
    void api.getMatch(active.secret, matchId).then((res) => {
      if (res.ok) setMatch(res.data.match)
      else setError(res.error)
    })
  }, [online, matchId])
  if (!active) return null

  async function decide(decision: MatchDecision) {
    if (!active || !canDecide(checked, busy)) return
    setBusy(true)
    setError(null)
    const res = await api.decideMatch(active.secret, matchId, decisionRequest(decision, decidedBy, note))
    setBusy(false)
    logEvent('match_decision', { decision, decided_by: decidedBy, status: res.ok ? 200 : res.status })
    if (res.ok) setDone({ decision, response: res.data })
    else {
      setError(res.error)
      if (res.status === 409) setClosed(true)
    }
  }

  const link = whatsappHref(done?.response.whatsapp_link)
  const family = done?.response.family
  const open = match?.status === 'open' && !closed

  return (
    <main class="screen">
      <BackLink />
      <h1 class="title">{t('match.title')}</h1>
      {!online && <p class="notice" role="status">{t('match.offline')}</p>}
      {error && <p class="notice notice-error" role="alert">{t(errorKey(error))}</p>}
      {online && !match && !error && <p class="body">{t('match.loading')}</p>}

      {done ? (
        <section class="card">
          {done.decision === 'accept' ? (
            <>
              <h2 class="subtitle">{t('match.accepted_title')}</h2>
              {family && (
                <>
                  <p class="body-strong">{t('match.family_contact', { name: family.name, relationship: family.relationship })}</p>
                  <p class="code">{family.phone}</p>
                </>
              )}
              {link && <a class="button" href={link} target="_blank" rel="noopener noreferrer">{t('match.whatsapp_open')}</a>}
              <p class="notice notice-warning body-strong" role="alert">{t('match.erase_warning')}</p>
            </>
          ) : (
            <p class="body">{t(done.decision === 'decline' ? 'match.done_declined' : 'match.done_not_the_person')}</p>
          )}
          <button type="button" class="button button-secondary" onClick={() => navigate('/inbox')}>{t('match.back')}</button>
        </section>
      ) : (
        match && (
          <>
            <p class="body">{t('match.intro')}</p>
            <p class="caption">
              {t(queueKey(match.mediator_kind))}
              {match.is_child ? ` · ${t('match.child')}` : ''}
              {' · '}
              {match.shelter_name ? t('match.shelter', { name: match.shelter_name }) : t('match.no_shelter')}
            </p>

            <section class="card">
              <h2 class="subtitle">{t('match.family_says')}</h2>
              <p class="body"><span class="caption">{t('match.f_name')}</span> {match.missing_person.name}</p>
              {match.missing_person.nickname && <p class="body"><span class="caption">{t('match.f_nickname')}</span> {match.missing_person.nickname}</p>}
              <p class="body"><span class="caption">{t('match.f_relationship')}</span> {match.family.relationship}</p>
              <p class="body"><span class="caption">{t('match.f_by', { name: match.family.name })}</span></p>
              {match.missing_person.notes && <p class="body"><span class="caption">{t('match.f_notes')}</span> {match.missing_person.notes}</p>}
            </section>

            <section class="card">
              <h2 class="subtitle">{t('match.registry')}</h2>
              <p class="body"><span class="caption">{t('match.r_name')}</span> {match.person.name ?? t('match.r_no_name')}</p>
              {match.person.social_name && <p class="body"><span class="caption">{t('match.r_social_name')}</span> {match.person.social_name}</p>}
              {match.person.wristband && <p class="body"><span class="caption">{t('match.r_wristband')}</span> <span class="code">{match.person.wristband}</span></p>}
            </section>

            {open ? (
              <section class="form">
                <label class="check">
                  <input type="checkbox" checked={checked} onChange={(e) => setChecked(e.currentTarget.checked)} />
                  {t('match.checked')}
                </label>
                {!checked && <p class="caption">{t('match.checked_hint')}</p>}
                <fieldset class="card fieldset">
                  <legend class="body-strong">{t('match.decided_by')}</legend>
                  {(['person', 'operator'] as DecidedBy[]).map((who) => (
                    <label class="check" key={who}>
                      <input type="radio" name="decided_by" checked={decidedBy === who} onChange={() => setDecidedBy(who)} />
                      {t(`match.by_${who}`)}
                    </label>
                  ))}
                </fieldset>
                <label class="field">
                  {t('match.note')}
                  <input type="text" value={note} maxLength={500} autocomplete="off" onInput={(e) => setNote(e.currentTarget.value)} />
                </label>
                {DECISIONS.map(({ decision, secondary }) => (
                  <div key={decision}>
                    <button
                      type="button"
                      class={secondary ? 'button button-secondary' : 'button'}
                      disabled={!canDecide(checked, busy)}
                      onClick={() => void decide(decision)}
                    >
                      {busy ? t('match.sending') : t(`match.${decision}`)}
                    </button>
                    <p class="caption">{t(`match.${decision}_help`)}</p>
                  </div>
                ))}
              </section>
            ) : (
              <p class="notice" role="status">{t('match.closed')}</p>
            )}
          </>
        )
      )}
    </main>
  )
}
