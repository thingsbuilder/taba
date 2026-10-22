import { useEffect, useState } from 'preact/hooks'
import type { DeliveryView, ReviewDecision } from '../api-phase5'
import type { AppContext } from '../app-context'
import { errorKey } from '../app-context'
import { useOnline } from '../hooks'
import { t } from '../i18n'
import { navigate } from '../nav'
import { reasonText, reviewErrorKey, reviewMeta } from '../review'
import { roleIn } from '../sessions'
import { BackLink } from './BackLink'

/** `/shelter/:id/review` (spec 3.5.3 item 8): the admin decides the deliveries in review. Needs signal. */
export function Review({ ctx, shelterId }: { ctx: AppContext; shelterId: string }) {
  const { active, api } = ctx
  const online = useOnline()
  const [list, setList] = useState<DeliveryView[] | null>(null)
  const [hasMore, setHasMore] = useState(false)
  const [notes, setNotes] = useState<Record<string, string>>({})
  const [error, setError] = useState<string | null>(null)
  const isAdmin = !!active && roleIn(active, shelterId) === 'admin'

  const load = async () => {
    if (!active) return
    const res = await api.listDeliveries(active.secret, shelterId, 'in_review')
    if (res.ok) {
      setList(res.data.deliveries)
      setHasMore(res.data.has_more === true)
    }
    else setError(t(reviewErrorKey(res.status) ?? errorKey(res.error)))
  }
  useEffect(() => {
    if (!active) return navigate('/login', true)
    if (!isAdmin) return navigate(`/shelter/${shelterId}`, true)
    if (online) void load()
  }, [online, shelterId])
  if (!active || !isAdmin) return null
  const secret = active.secret

  async function decide(id: string, decision: ReviewDecision) {
    setError(null)
    const res = await api.reviewDelivery(secret, id, { decision, note: (notes[id] ?? '').trim() })
    // 409: someone already decided it; the list is reloaded either way.
    if (!res.ok) setError(t(reviewErrorKey(res.status) ?? errorKey(res.error)))
    await load()
  }

  return (
    <main class="screen">
      <BackLink />
      <h1 class="title">{t('review.title')}</h1>
      {!online && <p class="notice notice-warning" role="status">{t('review.needs_signal')}</p>}
      {error && <p class="notice notice-error" role="alert">{error}</p>}
      {hasMore && <p class="notice notice-warning" role="status">{t('review.has_more')}</p>}
      {list?.length === 0 && <p class="body" role="status">{t('review.empty')}</p>}
      {list?.map((d) => (
        <section class="card notice-warning" key={d.id}>
          <p class="body-strong">{d.item} × {d.quantity}</p>
          <p class="body">{d.person_name ?? t('review.no_person')}</p>
          <p class="caption">{reviewMeta(d, t('review.approved_by'))}</p>
          {d.review_note && <p class="body">{t('review.note_of', { note: d.review_note })}</p>}
          {d.reasons.map((r, i) => {
            const text = reasonText(r)
            return <p class="body" key={i}>{t(text.key, text.params)}</p>
          })}
          <label class="field">
            {t('review.note')}
            <textarea class="field-text" rows={2} maxLength={500} value={notes[d.id] ?? ''} onInput={(e) => setNotes({ ...notes, [d.id]: e.currentTarget.value })} />
          </label>
          <button type="button" class="button" disabled={!online} onClick={() => void decide(d.id, 'reviewed_ok')}>{t('review.ok')}</button>
          <button type="button" class="button button-secondary" disabled={!online} onClick={() => void decide(d.id, 'irregular')}>{t('review.irregular')}</button>
        </section>
      ))}
    </main>
  )
}
