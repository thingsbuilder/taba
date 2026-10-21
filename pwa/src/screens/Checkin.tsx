import { useEffect, useRef, useState } from 'preact/hooks'
import type { CheckinResponse, LegalBasis } from '../api-phase3'
import type { AppContext } from '../app-context'
import { cameraSupported } from '../camera'
import { finishCapture, pollCapture, readDraft, startCapture } from '../captures'
import {
  type FormError, type PersonForm, bodyWithCapture, checkinErrorKey, emptyForm, enqueueCheckin, formToData, needsThumbnail, validateForm,
} from '../checkin'
import { effectiveBasis, rememberResponsible, responsibleOf } from '../consent'
import { useOnline } from '../hooks'
import { isDegraded } from '../degraded'
import { t } from '../i18n'
import { type Frame, photoForToken, thumbnailOf } from '../image'
import { navigate, useLocation } from '../nav'
import { setReturnTo } from '../return-to'
import { BackLink } from './BackLink'
import { CameraCapture } from './CameraCapture'
import { Consent } from './Consent'
import { PersonFields } from './PersonFields'

type Step = 'consent' | 'form' | 'method' | 'camera' | 'working' | 'done'

/** What waits across the Unico journey of path A (the page is reloaded when it comes back). */
interface Draft {
  id: string
  form: PersonForm
  basis: LegalBasis
}

interface Done {
  queued: boolean
  personId?: string
  response?: CheckinResponse
}

/** `/shelter/:id/checkin` (spec 3.4): consent, data, then one of the three ways to register. */
export function Checkin({ ctx, shelterId }: { ctx: AppContext; shelterId: string }) {
  const { active, api, db, queue } = ctx
  const online = useOnline()
  const { search } = useLocation()
  const captureId = new URLSearchParams(search).get('capture')
  const [step, setStep] = useState<Step>(captureId ? 'working' : 'consent')
  const [form, setForm] = useState<PersonForm>(emptyForm)
  const [errors, setErrors] = useState<FormError[]>([])
  const [error, setError] = useState<string | null>(null)
  const [basis, setBasis] = useState<LegalBasis>('consent')
  const [declined, setDeclined] = useState(false)
  const [unconfirmed, setUnconfirmed] = useState(false)
  const [responsible, setResponsible] = useState<string | null>(null)
  const [done, setDone] = useState<Done | null>(null)
  const [busy, setBusy] = useState(false)
  const id = useRef<string>(crypto.randomUUID())
  const resumed = useRef(false)

  const degraded = isDegraded(ctx.status)
  const canSelfie = online && !degraded
  const canPhoto = cameraSupported(navigator)

  useEffect(() => {
    if (!active) {
      void setReturnTo(db, `/shelter/${shelterId}/checkin`)
      navigate('/login', true)
    }
  }, [active])
  useEffect(() => {
    void db.get('shelters', shelterId).then((s) => setUnconfirmed(s?.confirmation !== 'confirmed'))
  }, [shelterId])
  useEffect(() => {
    void rememberResponsible(db, ctx.status).then(() => responsibleOf(db, ctx.status)).then(setResponsible)
  }, [ctx.status])

  // Coming back from the Unico journey of path A: the draft says what was being registered.
  useEffect(() => {
    if (!captureId || !active || resumed.current) return
    resumed.current = true
    void (async () => {
      const draft = await readDraft<Draft>(db, captureId)
      if (!draft) return navigate(`/shelter/${shelterId}/checkin`, true)
      id.current = draft.id
      setForm(draft.form)
      setBasis(draft.basis)
      const res = await pollCapture(() =>
        api.postCheckin(active.secret, shelterId, bodyWithCapture({
          id: draft.id, data: formToData(draft.form), legal_basis: effectiveBasis(draft.basis, draft.form.is_child), now: new Date(), capture_id: captureId,
        })),
      )
      await finishCapture(db, captureId)
      navigate(`/shelter/${shelterId}/checkin`, true)
      if (res.ok) {
        setDone({ queued: false, personId: res.data.person_id, response: res.data })
        setStep('done')
      } else {
        setError(res.error === 'timeout' ? 'selfie_timeout' : res.error)
        setStep('method')
      }
    })()
  }, [captureId, active?.volunteer_id])

  if (!active) return null
  const secret = active.secret

  const errorText = error ? t(checkinErrorKey(error)) : null

  async function enqueueAll(photo?: Frame) {
    setBusy(true)
    setError(null)
    const data = formToData(form)
    let blob: Blob | undefined
    let thumbnail: Blob | undefined
    if (photo) {
      const reduced = await photoForToken(photo)
      if (!reduced) {
        setBusy(false)
        return setError('photo_too_big')
      }
      blob = reduced
      if (needsThumbnail(data)) thumbnail = await thumbnailOf(photo)
    }
    await enqueueCheckin(queue, {
      id: id.current, data, legal_basis: effectiveBasis(basis, form.is_child), now: new Date(), shelterId, volunteerId: active!.volunteer_id,
      photo: blob, thumbnail, declined, degraded,
    })
    // The phone's own copy, marked pending until the server confirms (the queue swaps the id on sync).
    await db.put('people', {
      id: id.current, current_shelter_id: shelterId, name: data.name, social_name: data.social_name, wristband: data.wristband,
      level: blob && !degraded ? 'assisted' : 'no_biometrics', is_child: data.is_child, child_status: data.child_status,
      pending: true, updated_at: new Date().toISOString(),
    })
    setDone({ queued: true, personId: id.current })
    setStep('done')
    setBusy(false)
  }

  function submitData() {
    // Declining the selfie or choosing "no photo" is path C: it needs the name or the wristband.
    const found = validateForm(form, { needsIdentity: declined })
    setErrors(found)
    if (found.length > 0) return
    if (declined) void enqueueAll()
    else setStep('method')
  }

  function noPhoto() {
    const found = validateForm(form, { needsIdentity: true })
    setErrors(found)
    if (found.length > 0) return setStep('form')
    void enqueueAll()
  }

  async function selfie() {
    setBusy(true)
    setError(null)
    const draft: Draft = { id: id.current, form, basis }
    const res = await startCapture({ db, api }, secret, 'checkin', { shelter_id: shelterId }, `/shelter/${shelterId}/checkin`, draft)
    if (res.ok) return location.assign(res.link)
    setError(res.error)
    setBusy(false)
  }

  if (step === 'consent') {
    return (
      <>
        <div class="screen back-bar"><BackLink /></div>
        <Consent
          unconfirmed={unconfirmed}
          responsible={responsible}
          onChoose={(choice, chosen) => {
            setBasis(chosen)
            setDeclined(choice === 'decline')
            setStep('form')
          }}
        />
      </>
    )
  }

  if (step === 'working') {
    return (
      <main class="screen">
        <p class="body" role="status">{t('checkin.waiting_selfie')}</p>
      </main>
    )
  }

  if (step === 'done' && done) {
    const level = done.response?.level
    return (
      <main class="screen">
        <BackLink />
        <h1 class="title">{done.queued ? t('checkin.done_queued') : t('checkin.done')}</h1>
        {level && <p class="chip chip-inline">{t(`checkin.level_${level}`)}</p>}
        {done.response && <p class="body">{t(done.response.history.has_other_stays || done.response.history.stays?.length ? 'checkin.other_stays_yes' : 'checkin.other_stays_no')}</p>}
        {done.personId && !done.queued && (
          <button type="button" class="button" onClick={() => navigate(`/person/${done.personId}`)}>{t('checkin.open_person')}</button>
        )}
        <h2 class="subtitle">{t('checkin.household_question')}</h2>
        <button type="button" class="button button-secondary" onClick={() => navigate(`/shelter/${shelterId}/households/new`)}>{t('checkin.household_new')}</button>
        <button type="button" class="button button-secondary" onClick={() => navigate(`/shelter/${shelterId}/households/new`)}>{t('checkin.household_join')}</button>
        <button
          type="button"
          class="button button-secondary"
          onClick={() => {
            id.current = crypto.randomUUID()
            setForm(emptyForm())
            setDone(null)
            setErrors([])
            setStep('consent')
          }}
        >
          {t('checkin.another')}
        </button>
      </main>
    )
  }

  if (step === 'camera') {
    return (
      <main class="screen">
        <h1 class="title">{t('checkin.method_b')}</h1>
        <CameraCapture onPhoto={(frame) => { setStep('method'); void enqueueAll(frame) }} onCancel={() => setStep('method')} />
      </main>
    )
  }

  if (step === 'method') {
    return (
      <main class="screen">
        <button type="button" class="back-link back-button" onClick={() => setStep('form')}>{t('checkin.back')}</button>
        <h1 class="title">{t('checkin.method_title')}</h1>
        {errorText && <p class="notice notice-error" role="alert">{errorText}</p>}
        {degraded && <p class="notice notice-warning" role="status">{t('degraded.checkin_note')}</p>}
        {!online && <p class="notice" role="status">{t('checkin.needs_signal')}</p>}
        <button type="button" class="button" disabled={busy || !canSelfie} onClick={() => void selfie()}>{t('checkin.method_a')}</button>
        <p class="caption">{t('checkin.method_a_hint')}</p>
        <button type="button" class="button button-secondary" disabled={busy || !canPhoto} onClick={() => setStep('camera')}>{t('checkin.method_b')}</button>
        <p class="caption">{canPhoto ? t('checkin.method_b_hint') : t('checkin.camera_unsupported')}</p>
        <button type="button" class="button button-secondary" disabled={busy} onClick={noPhoto}>{t('checkin.method_c')}</button>
        <p class="caption">{t('checkin.method_c_hint')}</p>
        {busy && <p class="caption" role="status">{t('checkin.sending')}</p>}
      </main>
    )
  }

  return (
    <>
      <div class="screen back-bar"><BackLink /></div>
      <main class="screen">
        <h1 class="title">{t('checkin.step_data')}</h1>
        <PersonFields form={form} onChange={setForm} errors={errors} />
        {errors.length > 0 && <p class="notice notice-error" role="alert">{t('checkin.fix')}</p>}
        {errorText && <p class="notice notice-error" role="alert">{errorText}</p>}
        <button type="button" class="button" disabled={busy} onClick={submitData}>{declined ? t('checkin.method_c') : t('checkin.next')}</button>
      </main>
    </>
  )
}
