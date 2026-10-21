import { useEffect, useRef, useState } from 'preact/hooks'
import type { NearbyShelter } from '../api-phase4'
import type { AppContext } from '../app-context'
import { cameraSupported } from '../camera'
import { checkinErrorKey, needsThumbnail } from '../checkin'
import { isDegraded } from '../degraded'
import { type Position, MAX_ACCURACY_M, getPosition } from '../geo'
import { useOnline } from '../hooks'
import { t } from '../i18n'
import { type Frame, photoForToken, thumbnailOf } from '../image'
import { navigate } from '../nav'
import { type RescueError, type RescueForm, distanceText, emptyRescueForm, enqueueRescue, rescueBody, vacancyText, validateRescue } from '../rescue'
import { setReturnTo } from '../return-to'
import { BackLink } from './BackLink'
import { CameraCapture } from './CameraCapture'

type Nearby = { state: 'idle' | 'loading' | 'error' } | { state: 'ok'; shelters: NearbyShelter[] }

/** `/rescue` (spec 3.6): register a rescued person at the landing point. */
export function Rescue({ ctx }: { ctx: AppContext }) {
  const { active, api, db, queue } = ctx
  const online = useOnline()
  const [form, setForm] = useState<RescueForm>(emptyRescueForm)
  const [position, setPosition] = useState<Position | null>(null)
  const [locating, setLocating] = useState(true)
  const [frame, setFrame] = useState<Frame | null>(null)
  const [camera, setCamera] = useState(false)
  const [nearby, setNearby] = useState<Nearby>({ state: 'idle' })
  const [destination, setDestination] = useState('')
  const [errors, setErrors] = useState<RescueError[]>([])
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [done, setDone] = useState(false)
  const id = useRef(crypto.randomUUID())

  const degraded = isDegraded(ctx.status)
  const canPhoto = cameraSupported(navigator)

  useEffect(() => {
    if (!active) {
      void setReturnTo(db, '/rescue')
      navigate('/login', true)
    }
  }, [active])

  function locate() {
    setLocating(true)
    getPosition().then(setPosition, () => setPosition(null)).finally(() => setLocating(false))
  }
  useEffect(locate, [])

  // The nearby shelters need the point and a signal; the rescue itself does not (spec 3.6 item 4).
  useEffect(() => {
    if (!position || !online || !active) return setNearby({ state: 'idle' })
    let alive = true
    setNearby({ state: 'loading' })
    void api.nearbyShelters(active.secret, position.lat, position.lon).then((res) => {
      if (alive) setNearby(res.ok ? { state: 'ok', shelters: res.data.shelters } : { state: 'error' })
    })
    return () => {
      alive = false
    }
  }, [position?.lat, position?.lon, online, active?.volunteer_id])

  if (!active) return null

  async function submit() {
    const found = validateRescue(form, { position, hasPhoto: frame !== null })
    setErrors(found)
    if (found.length > 0 || !position) return
    setBusy(true)
    setError(null)
    let photo: Blob | undefined
    let thumbnail: Blob | undefined
    const body = rescueBody({ id: id.current, form, position, capturedAt: new Date(), destination: destination || undefined })
    if (frame) {
      const reduced = await photoForToken(frame)
      if (!reduced) {
        setBusy(false)
        return setError('photo_too_big')
      }
      photo = reduced
      if (needsThumbnail(body.data ?? {})) thumbnail = await thumbnailOf(frame)
    }
    await enqueueRescue(queue, { body, photo, thumbnail, degraded, volunteerId: active!.volunteer_id })
    setDone(true)
    setBusy(false)
  }

  function another() {
    id.current = crypto.randomUUID()
    setForm(emptyRescueForm())
    setFrame(null)
    setDestination('')
    setErrors([])
    setDone(false)
    locate()
  }

  if (done) {
    return (
      <main class="screen">
        <BackLink />
        <h1 class="title">{t('rescue.done')}</h1>
        <p class="body" role="status">{t('rescue.done_hint')}</p>
        <button type="button" class="button" onClick={another}>{t('rescue.another')}</button>
      </main>
    )
  }

  if (camera) {
    return (
      <main class="screen">
        <h1 class="title">{t('rescue.photo_title')}</h1>
        <CameraCapture onPhoto={(f) => { setFrame(f); setCamera(false) }} onCancel={() => setCamera(false)} />
      </main>
    )
  }

  const set = (key: keyof RescueForm, value: string) => setForm({ ...form, [key]: value })
  const field = (key: keyof RescueForm, label: string, extra: Record<string, string> = {}) => (
    <label class="field">
      {label}
      <input type="text" value={form[key]} autocomplete="off" onInput={(e) => set(key, e.currentTarget.value)} {...extra} />
    </label>
  )

  return (
    <>
      <div class="screen back-bar"><BackLink /></div>
      <main class="screen">
        <h1 class="title">{t('rescue.title')}</h1>
        {degraded && <p class="notice notice-warning" role="status">{t('rescue.degraded_note')}</p>}

        <section class="card">
          <h2 class="subtitle">{t('rescue.position_title')}</h2>
          {locating && <p class="caption" role="status">{t('rescue.position_getting')}</p>}
          {!locating && position && (
            <p class={position.accuracy > MAX_ACCURACY_M ? 'notice notice-warning' : 'caption'} role="status">
              {t(position.accuracy > MAX_ACCURACY_M ? 'rescue.position_weak' : 'rescue.position_ok', { m: Math.round(position.accuracy) })}
            </p>
          )}
          {!locating && !position && <p class="notice notice-error" role="alert">{t('rescue.position_error')}</p>}
          <button type="button" class="button button-secondary" disabled={locating} onClick={locate}>{t('rescue.position_again')}</button>
        </section>

        <section class="card">
          <h2 class="subtitle">{t('rescue.photo_title')}</h2>
          <p class="caption">{canPhoto ? t('rescue.photo_hint') : t('checkin.camera_unsupported')}</p>
          {frame && <p class="chip chip-inline" role="status">{t('rescue.photo_ready')}</p>}
          <button type="button" class="button" disabled={!canPhoto || busy} onClick={() => setCamera(true)}>{t(frame ? 'rescue.photo_retake' : 'rescue.photo_take')}</button>
          {!frame && <p class="caption">{t('rescue.photo_none')}</p>}
        </section>

        <div class="form">
          {field('name', t('rescue.name'), { maxLength: '100' })}
          {field('wristband', t('rescue.wristband'), { maxLength: '20' })}
          {errors.includes('name_or_wristband') && <p class="notice notice-error" role="alert">{t('checkin.err_name_or_wristband')}</p>}
          {field('note', t('rescue.note'), { maxLength: '500' })}
          {field('landing_point', t('rescue.landing_point'), { maxLength: '100' })}
        </div>

        <section class="card">
          <h2 class="subtitle">{t('rescue.shelters_title')}</h2>
          {!online && <p class="notice" role="status">{t('rescue.shelters_offline')}</p>}
          {online && nearby.state === 'loading' && <p class="caption" role="status">{t('rescue.shelters_loading')}</p>}
          {online && nearby.state === 'error' && <p class="notice notice-warning" role="status">{t('rescue.shelters_error')}</p>}
          {online && nearby.state === 'ok' && nearby.shelters.length === 0 && <p class="body">{t('rescue.shelters_none')}</p>}
          {nearby.state === 'ok' && nearby.shelters.length > 0 && (
            <fieldset class="fieldset">
              <label class="check">
                <input type="radio" name="destination" checked={destination === ''} onChange={() => setDestination('')} />
                {t('rescue.shelter_none_choice')}
              </label>
              {nearby.shelters.map((s) => {
                const distance = distanceText(s.distance_m)
                const vacancy = vacancyText(s)
                return (
                  <label class="check" key={s.id}>
                    <input type="radio" name="destination" checked={destination === s.id} onChange={() => setDestination(s.id)} />
                    <span>
                      <span class="body-strong">{s.name}</span> · {t(distance.key, distance.params)} · {t(s.confirmation === 'confirmed' ? 'shelter.confirmed' : 'shelter.unconfirmed')}
                      <br />
                      <span class="caption">{t(vacancy.key, vacancy.params)}</span>
                    </span>
                  </label>
                )
              })}
            </fieldset>
          )}
        </section>

        {error && <p class="notice notice-error" role="alert">{t(checkinErrorKey(error))}</p>}
        <button type="button" class="button" disabled={busy || locating} onClick={() => void submit()}>{busy ? t('rescue.saving') : t('rescue.submit')}</button>
      </main>
    </>
  )
}
