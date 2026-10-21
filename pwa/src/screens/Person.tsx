import { useEffect, useRef, useState } from 'preact/hooks'
import type { ChildStatus, GuardianLink, PersonView } from '../api-phase3'
import type { AppContext } from '../app-context'
import { finishCapture, pollCapture, readDraft, startCapture } from '../captures'
import { type PersonForm, checkinErrorKey, dataToForm, formToData, validateForm } from '../checkin'
import { CHILD_STATUSES } from '../checkin'
import { isProtectionOrDc } from '../exit'
import { useOnline } from '../hooks'
import { t } from '../i18n'
import { photoForToken } from '../image'
import { navigate, useLocation } from '../nav'
import { displayName } from '../people'
import { BackLink } from './BackLink'
import { CameraCapture } from './CameraCapture'
import { PersonFields } from './PersonFields'
import { nameText } from './People'

const GUARDIAN_LINKS: GuardianLink[] = ['declared', 'document_checked']

/** `/person/:id`: the record, by role (spec 4.11), with edit, late selfie, guardians and exit. */
export function Person({ ctx, personId }: { ctx: AppContext; personId: string }) {
  const { active, api, db, queue } = ctx
  const online = useOnline()
  const { search } = useLocation()
  const captureId = new URLSearchParams(search).get('capture')
  const [person, setPerson] = useState<PersonView | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const [thumb, setThumb] = useState<string | null>(null)
  const [editing, setEditing] = useState<PersonForm | null>(null)
  const [camera, setCamera] = useState(false)
  const [guardianNames, setGuardianNames] = useState<Record<string, string>>({})
  const [busy, setBusy] = useState(false)
  const resumed = useRef(false)

  const load = async () => {
    if (!active) return
    const res = await api.getPerson(active.secret, personId)
    if (res.ok) setPerson(res.data.person)
    else setError(res.error)
  }
  useEffect(() => {
    if (!active) return navigate('/login', true)
    if (online) void load()
  }, [online, personId])

  useEffect(() => {
    if (!active || !person?.has_thumbnail || !online) return
    let url: string | null = null
    void api.getThumbnail(active.secret, personId).then(async (res) => {
      if (res.ok) setThumb((url = URL.createObjectURL(await res.blob())))
    })
    return () => {
      if (url) URL.revokeObjectURL(url)
    }
  }, [person?.has_thumbnail, personId, online])

  useEffect(() => {
    if (!active || !person?.is_child) return
    void Promise.all(person.guardians.map(async (g) => [g.person_id, await api.getPerson(active.secret, g.person_id)] as const)).then((rows) =>
      setGuardianNames(Object.fromEntries(rows.map(([id, r]) => [id, r.ok ? nameText(displayName(r.data.person)) : t('person.no_name')]))),
    )
  }, [person?.id, person?.guardians.length])

  // Back from the Unico journey of the late selfie.
  useEffect(() => {
    if (!captureId || !active || resumed.current) return
    resumed.current = true
    void (async () => {
      const draft = await readDraft<{ personId: string }>(db, captureId)
      if (!draft) return navigate(`/person/${personId}`, true)
      const res = await pollCapture(() => api.putBiometrics(active.secret, personId, { capture_id: captureId }))
      await finishCapture(db, captureId)
      navigate(`/person/${personId}`, true)
      if (res.ok) {
        setNotice(t('person.biometrics_done', { level: t(`person.level.${res.data.level}`) }))
        await load()
      } else setError(res.error === 'timeout' ? 'selfie_timeout' : res.error)
    })()
  }, [captureId, active?.volunteer_id])

  if (!active) return null
  const secret = active.secret
  const shelterId = person?.current_shelter_id
  const isProt = isProtectionOrDc(active.global_roles)
  const errorText = error === 'selfie_timeout' ? t('checkin.selfie_timeout') : error ? t(checkinErrorKey(error)) : null

  async function saveEdit() {
    if (!editing || !person) return
    const found = validateForm(editing, { needsIdentity: false })
    if (found.length > 0) return setError('invalid_profile')
    // `is_child` and the child status are not edited here: the status changes only by protection or the Civil Defense.
    const { is_child: _c, child_status: _s, ...data } = formToData(editing)
    await queue.enqueue({
      method: 'PATCH',
      path: `/v1/people/${personId}`,
      body: { data, client_edited_at: new Date().toISOString() },
      volunteer_id: active!.volunteer_id,
      shelter_id: shelterId,
    })
    setEditing(null)
    setError(null)
    setNotice(t(online ? 'person.saved' : 'person.saved_queued'))
  }

  async function selfie() {
    setBusy(true)
    const res = await startCapture({ db, api }, secret, 'biometrics', { shelter_id: shelterId, expected_person_id: personId }, `/person/${personId}`, { personId })
    if (res.ok) return location.assign(res.link)
    setError(res.error)
    setBusy(false)
  }

  async function photo(frame: Parameters<typeof photoForToken>[0]) {
    setCamera(false)
    const blob = await photoForToken(frame)
    if (!blob) return setError('photo_too_big')
    await queue.enqueue(
      { method: 'PUT', path: `/v1/people/${personId}/biometrics`, body: {}, volunteer_id: active!.volunteer_id, shelter_id: shelterId },
      { photo: blob },
    )
    setNotice(t('person.biometrics_queued'))
  }

  async function changeStatus(status: ChildStatus) {
    const res = await api.patchPerson(secret, personId, { data: { child_status: status }, client_edited_at: new Date().toISOString() })
    if (res.ok) setPerson(res.data.person)
    else setError(res.error)
  }

  if (camera) {
    return (
      <main class="screen">
        <h1 class="title">{t('person.photo_offer')}</h1>
        <CameraCapture onPhoto={(f) => void photo(f)} onCancel={() => setCamera(false)} />
      </main>
    )
  }

  if (!person) {
    return (
      <main class="screen">
        <BackLink />
        <p class="body" role="status">{error ? '' : online ? t('person.loading') : t('person.needs_signal')}</p>
        {errorText && <p class="notice notice-error" role="alert">{errorText}</p>}
      </main>
    )
  }

  const name = displayName(person)
  const needsOn = Object.entries(person.needs).filter(([, on]) => on).map(([k]) => t(`checkin.need.${k}`))
  const v = person.vulnerabilities
  const vulnOn = [v.pregnant && t('checkin.pregnant'), v.lactating && t('checkin.lactating'), ...v.disability.map((d) => t(`checkin.disability.${d}`)), v.indigenous_people, v.migrant_language].filter(Boolean) as string[]

  return (
    <>
      <div class="screen back-bar"><BackLink /></div>
      <main class="screen">
        <h1 class="title">{nameText(name)}</h1>
        {person.social_name && person.name && <p class="caption">{person.name}</p>}
        <p class="chip chip-inline">{t(`person.level.${person.level}`)}</p>
        {person.birth_year !== undefined && <p class="body">{t('person.birth_year', { year: person.birth_year })}</p>}
        {thumb && <img class="thumbnail" src={thumb} alt={t('person.thumbnail_alt')} width="128" height="128" />}
        {notice && <p class="notice" role="status">{notice}</p>}
        {errorText && <p class="notice notice-error" role="alert">{errorText}</p>}

        {editing ? (
          <section class="card">
            <PersonFields form={editing} onChange={setEditing} showChild={false} />
            <button type="button" class="button" onClick={() => void saveEdit()}>{t('person.save')}</button>
            <button type="button" class="button button-secondary" onClick={() => setEditing(null)}>{t('person.cancel')}</button>
          </section>
        ) : (
          <>
            <section class="card">
              <h2 class="subtitle">{t('person.needs_title')}</h2>
              <p class="body">{needsOn.length ? needsOn.join(', ') : t('person.none')}</p>
              <h2 class="subtitle">{t('person.vulnerabilities_title')}</h2>
              <p class="body">{vulnOn.length ? vulnOn.join(', ') : t('person.none')}</p>
              <button type="button" class="button button-secondary" onClick={() => setEditing(dataToForm(person))}>{t('person.edit')}</button>
            </section>

            <section class="card">
              <h2 class="subtitle">{t('person.history_title')}</h2>
              {person.history.stays ? (
                <ul class="list">
                  {person.history.stays.map((s) => (
                    <li class="body" key={`${s.shelter_id}${s.entered_at}`}>
                      {s.shelter_name ?? s.shelter_id} · {new Date(s.entered_at).toLocaleDateString()}
                      {s.left_at ? ` – ${new Date(s.left_at).toLocaleDateString()}` : ` – ${t('person.stay_now')}`}
                      {s.exit_reason ? ` · ${t(`person.exit_reason.${s.exit_reason}`)}` : ''}
                    </li>
                  ))}
                </ul>
              ) : (
                <p class="body">{t(person.history.has_other_stays ? 'person.other_stays_yes' : 'person.other_stays_no')}</p>
              )}
            </section>

            {person.level !== 'verified' && online && (
              <section class="card">
                {person.biometrics_declined && <p class="caption">{t('person.declined')}</p>}
                <p class="caption">{t('person.selfie_hint')}</p>
                <button type="button" class="button button-secondary" disabled={busy || ctx.status?.unico === 'degraded'} onClick={() => void selfie()}>{t('person.selfie_offer')}</button>
                {person.level === 'no_biometrics' && (
                  <button type="button" class="button button-secondary" onClick={() => setCamera(true)}>{t('person.photo_offer')}</button>
                )}
              </section>
            )}

            {person.is_child && (
              <ChildSection
                ctx={ctx}
                person={person}
                guardianNames={guardianNames}
                isProt={isProt}
                onStatus={(s) => void changeStatus(s)}
                onChanged={setPerson}
                onError={setError}
              />
            )}

            {person.household_id && shelterId && (
              <button type="button" class="button button-secondary" onClick={() => navigate(`/shelter/${shelterId}/households/${person.household_id}`)}>
                {t('person.household')}
              </button>
            )}
            {shelterId && (
              <button type="button" class="button" onClick={() => navigate(`/shelter/${shelterId}/exit?person=${person.id}`)}>{t('person.exit')}</button>
            )}
          </>
        )}
      </main>
    </>
  )
}

/** Child status and guardians (spec 3.4.1). The server decides who may do what; the screen only hides what would be refused. */
function ChildSection({ ctx, person, guardianNames, isProt, onStatus, onChanged, onError }: {
  ctx: AppContext
  person: PersonView
  guardianNames: Record<string, string>
  isProt: boolean
  onStatus: (s: ChildStatus) => void
  onChanged: (p: PersonView) => void
  onError: (code: string) => void
}) {
  const { active, api } = ctx
  const online = useOnline()
  const [adding, setAdding] = useState(false)
  const [options, setOptions] = useState<{ id: string; label: string }[]>([])
  const [guardian, setGuardian] = useState('')
  const [link, setLink] = useState<GuardianLink>('document_checked')

  async function openAdd() {
    setAdding(true)
    if (!active || !person.current_shelter_id) return
    const res = await api.listPeople(active.secret, person.current_shelter_id)
    if (res.ok) {
      setOptions(res.data.people.filter((p) => p.level === 'verified' && !p.is_child && p.id !== person.id).map((p) => ({ id: p.id, label: nameText(displayName(p)) })))
    }
  }

  async function save() {
    if (!active || !guardian) return
    const res = await api.postGuardian(active.secret, person.id, { guardian_id: guardian, link })
    if (!res.ok) return onError(res.error)
    onChanged(res.data.person)
    setAdding(false)
  }

  return (
    <section class="card">
      <h2 class="subtitle">{t('child.title')}</h2>
      {person.child_status && <p class="body-strong">{t('child.status_label', { status: t(`child.status.${person.child_status}`) })}</p>}
      {isProt && online ? (
        <label class="field">
          {t('child.status_change')}
          <select value={person.child_status ?? ''} onChange={(e) => onStatus(e.currentTarget.value as ChildStatus)}>
            {CHILD_STATUSES.map((s) => <option value={s} key={s}>{t(`child.status.${s}`)}</option>)}
          </select>
        </label>
      ) : (
        <p class="caption">{t('child.status_change_only')}</p>
      )}

      <h3 class="body-strong">{t('child.guardians_title')}</h3>
      {person.guardians.length === 0 && <p class="body">{t('child.guardians_none')}</p>}
      <ul class="list">
        {person.guardians.map((g) => (
          <li class="body" key={g.person_id}>
            {guardianNames[g.person_id] ?? '…'} · {t(`child.link.${g.link}`)}
            {g.checked_at ? ` · ${t('child.checked', { when: new Date(g.checked_at).toLocaleDateString() })}` : ''}
          </li>
        ))}
      </ul>

      {online && !adding && <button type="button" class="button button-secondary" onClick={() => void openAdd()}>{t('child.guardian_add')}</button>}
      {adding && (
        <div class="form">
          <p class="caption">{t('child.guardian_only_verified')}</p>
          {options.length === 0 ? (
            <p class="notice" role="status">{t('child.guardian_none_available')}</p>
          ) : (
            <>
              <label class="field">
                {t('child.guardian_pick')}
                <select value={guardian} onChange={(e) => setGuardian(e.currentTarget.value)}>
                  <option value="" />
                  {options.map((o) => <option value={o.id} key={o.id}>{o.label}</option>)}
                </select>
              </label>
              <label class="field">
                {t('child.guardian_link')}
                <select value={link} onChange={(e) => setLink(e.currentTarget.value as GuardianLink)}>
                  {GUARDIAN_LINKS.map((l) => <option value={l} key={l}>{t(`child.link.${l}`)}</option>)}
                </select>
              </label>
              <button type="button" class="button" disabled={!guardian} onClick={() => void save()}>{t('child.guardian_save')}</button>
            </>
          )}
        </div>
      )}
    </section>
  )
}
