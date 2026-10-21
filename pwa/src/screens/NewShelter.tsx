import { useEffect, useState } from 'preact/hooks'
import type { AppContext } from '../app-context'
import { errorKey } from '../app-context'
import { type Position, accuracyOk, getPosition } from '../geo'
import { t } from '../i18n'
import { BackLink } from './BackLink'
import { navigate } from '../nav'
import { createShelter } from '../shelters'

/** `/shelters/new` (spec 3.2): GPS with accuracy, address from Nominatim, name and capacity, then the queue. */
export function NewShelter({ ctx }: { ctx: AppContext }) {
  const { active } = ctx
  const [position, setPosition] = useState<Position | null>(null)
  const [gpsFailed, setGpsFailed] = useState(false)
  const [name, setName] = useState('')
  const [capacity, setCapacity] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const locate = () => {
    setPosition(null)
    setGpsFailed(false)
    getPosition().then(setPosition, () => setGpsFailed(true))
  }
  useEffect(() => {
    if (!active) navigate('/login', true)
    else locate()
  }, [])
  if (!active) return null

  const weak = position !== null && !accuracyOk(position.accuracy)

  async function submit(e: Event) {
    e.preventDefault()
    if (!position || !active) return
    setBusy(true)
    setError(null)
    const address = await ctx.nominatim.reverse(position.lat, position.lon)
    const result = await createShelter(
      ctx.db, ctx.queue, active.volunteer_id,
      { name, position, capacity: capacity.trim() ? Number(capacity) : undefined, address: address ?? undefined },
      accuracyOk,
    )
    if (!result.ok) {
      setError(result.error)
      return setBusy(false)
    }
    if (!address) await ctx.nominatim.resolveFor(result.id, position.lat, position.lon, active.volunteer_id)
    navigate(`/shelter/${result.id}`)
  }

  return (
    <main class="screen">
      <BackLink />
      <h1 class="title">{t('shelter_new.title')}</h1>
      {!position && !gpsFailed && <p class="body" role="status">{t('shelter_new.locating')}</p>}
      {gpsFailed && <p class="notice notice-error" role="alert">{t('shelter_new.gps_failed')}</p>}
      {weak && <p class="notice notice-warning" role="alert">{t('shelter_new.weak_gps', { m: Math.round(position.accuracy) })}</p>}
      {position && !weak && <p class="notice" role="status">{t('shelter_new.gps_ok', { m: Math.round(position.accuracy) })}</p>}
      {(weak || gpsFailed) && <button type="button" class="button button-secondary" onClick={locate}>{t('shelter_new.retry_gps')}</button>}
      <form class="form" onSubmit={(e) => void submit(e)}>
        <label class="field">
          {t('shelter_new.name')}
          <input type="text" maxLength={100} value={name} onInput={(e) => setName(e.currentTarget.value)} />
        </label>
        <label class="field">
          {t('shelter_new.capacity')}
          <input type="number" inputMode="numeric" min={1} value={capacity} onInput={(e) => setCapacity(e.currentTarget.value)} />
        </label>
        {error && <p class="notice notice-error" role="alert">{t(errorKey(error))}</p>}
        <button type="submit" class="button" disabled={busy || !position || weak}>{t('shelter_new.create')}</button>
      </form>
    </main>
  )
}
