import { useEffect, useRef, useState } from 'preact/hooks'
import type { SearchLinkCreated } from '../api-phase4'
import type { AppContext } from '../app-context'
import { errorKey } from '../app-context'
import { cameraSupported } from '../camera'
import { getPosition } from '../geo'
import { checkinErrorKey } from '../checkin'
import { useOnline } from '../hooks'
import { getLanguage, t } from '../i18n'
import { type Frame, photoForToken } from '../image'
import { navigate } from '../nav'
import { queueErrorKey } from '../queue-errors'
import { setReturnTo } from '../return-to'
import {
  FAMILY_LANGUAGES, MAX_PHOTOS, type SearchError, type SearchForm, type ShelterChoice, canSearch, confirmedShelters, emptySearchForm, enqueueSearch, hasSearchRole,
  reducePhotos, shareLink, shelterChoices, takeSearchReply, validateSearch,
} from '../search'
import { BackLink } from './BackLink'
import { CameraCapture } from './CameraCapture'

interface Photo {
  blob: Blob
  url: string
}

/** What the screen knows of the search it sent: waiting in the queue, answered, or refused and waiting for a person. */
type Sent = { itemId: string; id: string; photos: number; usable: number | null; refusedWith: string | null }

/** `/search` (spec 3.7): register a search, or send the family a link. */
export function Search({ ctx }: { ctx: AppContext }) {
  const { active, db, queue } = ctx
  const [form, setForm] = useState<SearchForm>(emptySearchForm)
  const [photos, setPhotos] = useState<Photo[]>([])
  const [camera, setCamera] = useState(false)
  const [errors, setErrors] = useState<SearchError[]>([])
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [sent, setSent] = useState<Sent | null>(null)
  const id = useRef(crypto.randomUUID())
  const gallery = useRef<HTMLInputElement>(null)

  useEffect(() => {
    if (!active) {
      void setReturnTo(db, '/search')
      navigate('/login', true)
    }
  }, [active])
  useEffect(() => () => photos.forEach((p) => URL.revokeObjectURL(p.url)), [])

  // The queue sends the search; what it answered (`usable_photos`) or why it refused is read from it.
  useEffect(() => {
    if (!sent || sent.usable !== null || sent.refusedWith) return
    const watch = async () => {
      const item = (await queue.list()).find((i) => i.id === sent.itemId)
      if (item) {
        if (item.state === 'needs_action') setSent((s) => (s && { ...s, refusedWith: item.error ?? 'generic' }))
        return
      }
      const usable = await takeSearchReply(db, sent.id)
      if (usable !== null) setSent((s) => (s && { ...s, usable }))
    }
    void watch()
    return queue.subscribe(() => void watch())
  }, [sent?.itemId, sent?.usable, sent?.refusedWith])

  if (!active) return null
  const session = active

  async function addPhotos(blobs: Blob[] | null) {
    if (!blobs) return setError('photo_too_big')
    setError(null)
    setPhotos((current) => [...current, ...blobs.slice(0, MAX_PHOTOS - current.length).map((blob) => ({ blob, url: URL.createObjectURL(blob) }))])
  }

  async function fromGallery(files: FileList | null) {
    if (!files || files.length === 0) return
    setBusy(true)
    try {
      await addPhotos(await reducePhotos(Array.from(files).slice(0, MAX_PHOTOS - photos.length)))
    } catch {
      setError('invalid_image')
    }
    setBusy(false)
    if (gallery.current) gallery.current.value = ''
  }

  async function fromCamera(frame: Frame) {
    setCamera(false)
    setBusy(true)
    const reduced = await photoForToken(frame)
    await addPhotos(reduced ? [reduced] : null)
    setBusy(false)
  }

  function removePhoto(index: number) {
    URL.revokeObjectURL(photos[index]!.url)
    setPhotos(photos.filter((_, i) => i !== index))
  }

  async function submit() {
    const found = validateSearch(form, photos.length)
    setErrors(found)
    if (found.length > 0) return
    setBusy(true)
    const item = await enqueueSearch(queue, { id: id.current, form, photos: photos.map((p) => p.blob), volunteerId: session.volunteer_id })
    setSent({ itemId: item.id, id: id.current, photos: photos.length, usable: null, refusedWith: null })
    photos.forEach((p) => URL.revokeObjectURL(p.url))
    setPhotos([])
    setBusy(false)
  }

  function another() {
    id.current = crypto.randomUUID()
    setForm(emptySearchForm())
    setErrors([])
    setSent(null)
  }

  if (camera) {
    return (
      <main class="screen">
        <h1 class="title">{t('search.photos_title')}</h1>
        <CameraCapture onPhoto={(f) => void fromCamera(f)} onCancel={() => setCamera(false)} />
      </main>
    )
  }

  if (sent) {
    return (
      <main class="screen">
        <BackLink />
        <p class="notice" role="note">{t('search.notice')}</p>
        {sent.refusedWith ? (
          <>
            <p class="notice notice-error" role="alert">{t(queueErrorKey(sent.refusedWith))}</p>
            <button type="button" class="button" onClick={() => { setSent({ ...sent, refusedWith: null }); void queue.retry(sent.itemId) }}>{t('search.retry')}</button>
            <button type="button" class="button button-secondary" onClick={() => { void queue.discard(sent.itemId); another() }}>{t('search.discard')}</button>
          </>
        ) : (
          <>
            <h1 class="title">{sent.usable === null ? t('search.queued') : t('search.registered')}</h1>
            {sent.usable !== null && (
              <>
                <p class="body" role="status">{t('search.usable', { usable: sent.usable, sent: sent.photos })}</p>
                {sent.usable < sent.photos && <p class="body">{t('search.usable_more')}</p>}
              </>
            )}
            <button type="button" class="button" onClick={another}>{t('search.another')}</button>
          </>
        )}
      </main>
    )
  }

  const set = (key: keyof SearchForm, value: string) => setForm({ ...form, [key]: value })
  const field = (key: Exclude<keyof SearchForm, 'language'>, label: string, extra: Record<string, string> = {}) => (
    <>
      <label class="field">
        {label}
        <input type="text" value={form[key]} aria-invalid={errors.includes(key as SearchError)} autocomplete="off" onInput={(e) => set(key, e.currentTarget.value)} {...extra} />
      </label>
      {errors.includes(key as SearchError) && <p class="notice notice-error" role="alert">{t(`search.err_${key}`)}</p>}
    </>
  )

  return (
    <>
      <div class="screen back-bar"><BackLink /></div>
      <main class="screen">
        <p class="notice" role="note">{t('search.notice')}</p>
        <h1 class="title">{t('search.title')}</h1>
        {!canSearch(session) && <p class="notice notice-error" role="alert">{t('error.forbidden')}</p>}

        <section class="card">
          <h2 class="subtitle">{t('search.missing_title')}</h2>
          {field('name', t('search.name'), { maxLength: '100' })}
          {field('nickname', t('search.nickname'), { maxLength: '100' })}
          <label class="field">
            {t('search.notes')}
            <textarea class="field-text" rows={3} maxLength={1000} value={form.notes} onInput={(e) => set('notes', e.currentTarget.value)} />
          </label>
        </section>

        <section class="card">
          <h2 class="subtitle">{t('search.family_title')}</h2>
          {field('family_name', t('search.family_name'), { maxLength: '100' })}
          {field('phone', t('search.phone'), { type: 'tel', inputMode: 'tel', maxLength: '30' })}
          {field('relationship', t('search.relationship'), { maxLength: '50' })}
          <label class="field">
            {t('search.language')}
            <select value={form.language} onChange={(e) => set('language', e.currentTarget.value)}>
              {FAMILY_LANGUAGES.map((l) => <option key={l} value={l}>{t(`search.language.${l}`)}</option>)}
            </select>
          </label>
        </section>

        <section class="card">
          <h2 class="subtitle">{t('search.photos_title')}</h2>
          <p class="caption">{t('search.photos_hint')}</p>
          <p class="caption">{t('search.photos_kept')}</p>
          <p class="caption" role="status">{t('search.photos_count', { n: photos.length })}</p>
          <ul class="list">
            {photos.map((p, i) => (
              <li key={p.url}>
                <img src={p.url} alt="" style="width:72px;height:72px;object-fit:cover;border-radius:8px" />
                <button type="button" class="button button-secondary" onClick={() => removePhoto(i)}>{t('search.photo_remove', { n: i + 1 })}</button>
              </li>
            ))}
          </ul>
          {/* The one place the gallery is allowed (spec 6.1.3). */}
          <input ref={gallery} type="file" accept="image/*" multiple hidden onChange={(e) => void fromGallery(e.currentTarget.files)} />
          <button type="button" class="button button-secondary" disabled={busy || photos.length >= MAX_PHOTOS} onClick={() => gallery.current?.click()}>{t('search.photos_gallery')}</button>
          <button type="button" class="button button-secondary" disabled={busy || photos.length >= MAX_PHOTOS || !cameraSupported(navigator)} onClick={() => setCamera(true)}>{t('search.photos_camera')}</button>
          {busy && <p class="caption" role="status">{t('search.photos_reducing')}</p>}
          {errors.includes('photos') && <p class="notice notice-error" role="alert">{t('search.err_photos')}</p>}
        </section>

        {error && <p class="notice notice-error" role="alert">{t(checkinErrorKey(error))}</p>}
        <button type="button" class="button" disabled={busy || !canSearch(session)} onClick={() => void submit()}>{t('search.submit')}</button>

        <FamilyLink ctx={ctx} />
      </main>
    </>
  )
}

/** "Enviar link para a família" (spec 3.7 item 11): needs signal; the volunteer never reads what the family sends. */
function FamilyLink({ ctx }: { ctx: AppContext }) {
  const online = useOnline()
  const own = ctx.active ? confirmedShelters(ctx.active) : []
  const [near, setNear] = useState<ShelterChoice[]>([])
  const shelters = shelterChoices(own, near.map((s) => ({ id: s.shelter_id, name: s.name, confirmation: 'confirmed' as const })))
  const [shelterId, setShelterId] = useState(own[0]?.shelter_id ?? '')
  const [looking, setLooking] = useState(false)
  const [link, setLink] = useState<SearchLinkCreated | null>(null)
  const [message, setMessage] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  if (!ctx.active || (own.length === 0 && !hasSearchRole(ctx.active))) return null
  const secret = ctx.active.secret

  /** The Civil Defense, a mediator or protection name any confirmed shelter: those near where they are. */
  async function lookNearby() {
    setLooking(true)
    setError(null)
    try {
      const position = await getPosition()
      const res = await ctx.api.nearbyShelters(secret, position.lat, position.lon)
      if (!res.ok) return setError(res.error)
      const found = shelterChoices([], res.data.shelters)
      setNear(found)
      if (found.length === 0) setError('search.link_none_nearby')
      else if (!shelterId) setShelterId(found[0]!.shelter_id)
    } catch {
      setError('search.link_no_position')
    } finally {
      setLooking(false)
    }
  }

  async function share(created: SearchLinkCreated) {
    const result = await shareLink(navigator, t('search.link_share_text'), created.url)
    setMessage(result === 'shared' ? 'search.link_shared' : result === 'copied' ? 'search.link_copied' : result === 'unsupported' ? 'search.link_unsupported' : null)
  }

  async function create() {
    setBusy(true)
    setError(null)
    setMessage(null)
    const res = await ctx.api.createSearchLink(secret, { shelter_id: shelterId })
    setBusy(false)
    if (!res.ok) return setError(res.error)
    setLink(res.data)
    await share(res.data)
  }

  return (
    <section class="card">
      <h2 class="subtitle">{t('search.link_title')}</h2>
      <p class="caption">{t('search.link_hint')}</p>
      {hasSearchRole(ctx.active) && (
        <button type="button" class="button button-secondary" disabled={!online || looking} onClick={() => void lookNearby()}>{looking ? t('search.link_looking') : t('search.link_other')}</button>
      )}
      {shelters.length > 1 && (
        <label class="field">
          {t('search.link_shelter')}
          <select value={shelterId} onChange={(e) => setShelterId(e.currentTarget.value)}>
            {shelters.map((s) => <option key={s.shelter_id} value={s.shelter_id}>{s.name}</option>)}
          </select>
        </label>
      )}
      {!online && <p class="notice" role="status">{t('search.link_needs_signal')}</p>}
      {shelters.length === 0 && <p class="caption">{t('search.link_pick_shelter')}</p>}
      {error && <p class="notice notice-error" role="alert">{t(error.startsWith('search.') ? error : errorKey(error))}</p>}
      <button type="button" class="button button-secondary" disabled={!online || busy || shelterId === ''} onClick={() => void create()}>{busy ? t('search.link_creating') : t('search.link_create')}</button>
      {link && (
        <>
          <input type="text" readOnly value={link.url} aria-label={t('search.link_title')} onFocus={(e) => e.currentTarget.select()} />
          <p class="caption">{t('search.link_valid_until', { time: new Date(link.expires_at).toLocaleString(getLanguage(), { dateStyle: 'short', timeStyle: 'short' }) })}</p>
          <button type="button" class="button button-secondary" onClick={() => void share(link)}>{t('search.link_title')}</button>
        </>
      )}
      {message && <p class="notice" role="status">{t(message)}</p>}
    </section>
  )
}
