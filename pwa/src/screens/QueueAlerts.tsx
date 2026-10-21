import { useEffect, useState } from 'preact/hooks'
import type { AppContext } from '../app-context'
import type { QueueItem } from '../db'
import { t } from '../i18n'
import { useConflictNames } from '../pending-hook'
import { pendingViews } from '../pending'
import { photoForToken } from '../image'
import { photoRefusalActions, phase3Label } from '../queue-errors'
import { CameraCapture } from './CameraCapture'
import { type TakeOverOffer, nameOf, rememberNames, takeOverOffers } from '../takeover'

const DAY_MS = 24 * 3600_000

/** Under the top bar, on every screen: items that wait for a person, offers to take over, items stuck for a day (spec 9.2). */
export function QueueAlerts({ ctx }: { ctx: AppContext }) {
  const { db, queue, active, sessions } = ctx
  const [items, setItems] = useState<QueueItem[]>([])
  const [names, setNames] = useState<Record<string, string>>({})
  const [retaking, setRetaking] = useState<string | null>(null)
  const [photoError, setPhotoError] = useState<string | null>(null)

  useEffect(() => {
    const load = () => void queue.list().then(setItems)
    load()
    return queue.subscribe(load)
  }, [queue])
  useEffect(() => {
    void rememberNames(db, sessions)
  }, [sessions])

  const conflicts = useConflictNames(db, items)
  const offers = takeOverOffers(items, active, sessions)
  const offerKey = offers.map((o) => o.fromVolunteerId).join(',')
  useEffect(() => {
    void Promise.all(offers.map(async (o) => [o.fromVolunteerId, await nameOf(db, o.fromVolunteerId)] as const)).then((pairs) =>
      setNames(Object.fromEntries(pairs.filter(([, n]) => n !== null) as [string, string][])),
    )
  }, [offerKey])

  if (!active) return null
  const photoActions = (id: string) => {
    const item = items.find((i) => i.id === id)
    return item ? photoRefusalActions(item) : null
  }
  const phase3 = new Set(items.filter((i) => phase3Label(i)).map((i) => i.id))
  const waiting = pendingViews(items, undefined, conflicts).filter((p) => phase3.has(p.id))
  const stale = items.some((i) => i.state === 'pending' && Date.now() - i.created_at > DAY_MS)

  async function newPhoto(frame: Parameters<typeof photoForToken>[0]) {
    const id = retaking
    setRetaking(null)
    const blob = await photoForToken(frame)
    if (!blob) return setPhotoError('checkin.photo_too_big')
    if (id) await queue.replacePhoto(id, blob)
  }

  if (retaking) {
    return (
      <section class="card">
        <CameraCapture onPhoto={(f) => void newPhoto(f)} onCancel={() => setRetaking(null)} />
      </section>
    )
  }

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
          {p.errorKey && <p class="body">{t(p.errorKey, p.errorParams)}</p>}
          {photoActions(p.id) ? (
            <>
              {photoActions(p.id)!.again ? (
                <button type="button" class="button" onClick={() => { setPhotoError(null); setRetaking(p.id) }}>{t('queue.photo_again')}</button>
              ) : (
                <p class="caption">{t('queue.photo_last')}</p>
              )}
              <button type="button" class="button button-secondary" onClick={() => void queue.registerWithoutPhoto(p.id)}>{t('queue.photo_without')}</button>
            </>
          ) : (
            p.state === 'needs_action' && p.retryable && <button type="button" class="button button-secondary" onClick={() => void queue.retry(p.id)}>{t('queue.retry')}</button>
          )}
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
      {photoError && <p class="notice notice-error" role="alert">{t(photoError)}</p>}
      {stale && <p class="notice" role="status">{t('queue.stale')}</p>}
    </>
  )
}
