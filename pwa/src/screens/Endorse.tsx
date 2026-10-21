import { useEffect, useState } from 'preact/hooks'
import type { AppContext } from '../app-context'
import { errorKey } from '../app-context'
import { type Position, accuracyOk, getPosition } from '../geo'
import { t } from '../i18n'
import { navigate } from '../nav'
import { setReturnTo } from '../return-to'
import { saveView } from '../shelters'

/** `/endorse/:code` (spec 3.9): the endorsing admin picks their confirmed shelter and sends both positions. */
export function Endorse({ ctx, code }: { ctx: AppContext; code: string }) {
  const { active } = ctx
  const eligible = active?.memberships?.filter((m) => m.role === 'admin' && m.confirmation === 'confirmed') ?? []
  const [shelterId, setShelterId] = useState(eligible[0]?.shelter_id ?? '')
  const [position, setPosition] = useState<Position | null>(null)
  const [gpsFailed, setGpsFailed] = useState(false)
  const [error, setError] = useState<{ code: string; distance?: number } | null>(null)
  const [done, setDone] = useState(false)
  const [busy, setBusy] = useState(false)

  const locate = () => {
    setPosition(null)
    setGpsFailed(false)
    getPosition().then(setPosition, () => setGpsFailed(true))
  }
  useEffect(() => {
    if (!active) void setReturnTo(ctx.db, `/endorse/${code}`).then(() => navigate('/login', true))
    else locate()
  }, [])
  if (!active) return null

  const weak = position !== null && !accuracyOk(position.accuracy)

  async function send() {
    if (!position || !active) return
    setBusy(true)
    setError(null)
    const res = await ctx.api.endorse(active.secret, { code, endorser_shelter_id: shelterId, ...position })
    setBusy(false)
    if (res.ok) {
      await saveView(ctx.db, res.data.shelter)
      return setDone(true)
    }
    const distance = (res.detail as { distance_m?: number } | undefined)?.distance_m
    setError({ code: res.error, distance })
  }

  return (
    <main class="screen">
      <h1 class="title">{t('endorse.title')}</h1>
      {done && <p class="notice" role="status">{t('endorse.done')}</p>}
      {!done && eligible.length === 0 && <p class="notice notice-warning" role="alert">{t('endorse.no_shelter')}</p>}
      {!done && eligible.length > 0 && (
        <>
          <p class="body">{t('endorse.body')}</p>
          <label class="field">
            {t('endorse.pick')}
            <select value={shelterId} onChange={(e) => setShelterId(e.currentTarget.value)}>
              {eligible.map((m) => <option value={m.shelter_id}>{m.name}</option>)}
            </select>
          </label>
          {!position && !gpsFailed && <p class="body" role="status">{t('shelter_new.locating')}</p>}
          {gpsFailed && <p class="notice notice-error" role="alert">{t('shelter_new.gps_failed')}</p>}
          {weak && <p class="notice notice-warning" role="alert">{t('shelter_new.weak_gps', { m: Math.round(position.accuracy) })}</p>}
          {(weak || gpsFailed) && <button type="button" class="button button-secondary" onClick={locate}>{t('shelter_new.retry_gps')}</button>}
          {error && (
            <p class="notice notice-error" role="alert">
              {error.code === 'too_far' && error.distance !== undefined ? t('endorse.too_far', { m: Math.round(error.distance) }) : t(errorKey(error.code))}
            </p>
          )}
          <button type="button" class="button" disabled={busy || !position || weak || !shelterId} onClick={() => void send()}>{t('endorse.send')}</button>
        </>
      )}
    </main>
  )
}
