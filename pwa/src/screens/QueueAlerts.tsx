import { useEffect, useState } from 'preact/hooks'
import type { AppContext } from '../app-context'
import type { QueueItem } from '../db'
import { t } from '../i18n'
import { pendingViews } from '../pending'
import { phase3Label } from '../queue-errors'
import { type TakeOverOffer, nameOf, rememberNames, takeOverOffers } from '../takeover'

const DAY_MS = 24 * 3600_000

/** Under the top bar, on every screen: items that wait for a person, offers to take over, items stuck for a day (spec 9.2). */
export function QueueAlerts({ ctx }: { ctx: AppContext }) {
  const { db, queue, active, sessions } = ctx
  const [items, setItems] = useState<QueueItem[]>([])
  const [names, setNames] = useState<Record<string, string>>({})

  useEffect(() => {
    const load = () => void queue.list().then(setItems)
    load()
    return queue.subscribe(load)
  }, [queue])
  useEffect(() => {
    void rememberNames(db, sessions)
  }, [sessions])

  const offers = takeOverOffers(items, active, sessions)
  const offerKey = offers.map((o) => o.fromVolunteerId).join(',')
  useEffect(() => {
    void Promise.all(offers.map(async (o) => [o.fromVolunteerId, await nameOf(db, o.fromVolunteerId)] as const)).then((pairs) =>
      setNames(Object.fromEntries(pairs.filter(([, n]) => n !== null) as [string, string][])),
    )
  }, [offerKey])

  if (!active) return null
  const phase3 = new Set(items.filter((i) => phase3Label(i)).map((i) => i.id))
  const waiting = pendingViews(items).filter((p) => phase3.has(p.id))
  const stale = items.some((i) => i.state === 'pending' && Date.now() - i.created_at > DAY_MS)

  const takeOver = (o: TakeOverOffer) => void queue.takeOver(o.fromVolunteerId, active.volunteer_id, o.shelterIds)
  return (
    <>
      {offers.map((o) => (
        <section class="card" role="status" key={o.fromVolunteerId}>
          <p class="body">{t('queue.takeover_hint', { n: o.count })}</p>
          <button type="button" class="button" onClick={() => takeOver(o)}>
            {t('queue.takeover', { name: names[o.fromVolunteerId] ?? t('queue.takeover_unknown') })}
          </button>
        </section>
      ))}
      {waiting.map((p) => (
        <section class="card" role="alert" key={p.id}>
          <p class="body-strong">{t(p.labelKey)}</p>
          {p.errorKey && <p class="body">{t(p.errorKey)}</p>}
          {p.state === 'needs_action' && <button type="button" class="button button-secondary" onClick={() => void queue.retry(p.id)}>{t('queue.retry')}</button>}
          <button
            type="button"
            class="button button-secondary"
            onClick={() => {
              if (confirm(t('queue.discard_confirm'))) void queue.discard(p.id)
            }}
          >
            {t('queue.discard')}
          </button>
        </section>
      ))}
      {stale && <p class="notice" role="status">{t('queue.stale')}</p>}
    </>
  )
}
