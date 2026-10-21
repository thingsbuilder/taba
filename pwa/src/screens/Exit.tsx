import { useEffect, useRef, useState } from 'preact/hooks'
import type { ExitDestination, ExitRequest, PersonSummary, PersonView } from '../api-phase3'
import type { AppContext } from '../app-context'
import { finishCapture, pollCapture, readDraft, startCapture } from '../captures'
import { EXIT_DESTINATIONS, buildExit, canRegisterChildExit, childExitRoute, exitErrorKey, isProtectionOrDc, queuedExit } from '../exit'
import { useOnline } from '../hooks'
import { t } from '../i18n'
import { navigate, useLocation } from '../nav'
import { cachedPeople, displayName, matchesQuery } from '../people'
import { BackLink } from './BackLink'
import { PersonRow } from './People'
import { nameText } from './People'

/** `/shelter/:id/exit` (spec 3.8): who leaves and where to. A child leaves with the guardian's selfie or by exception. */
export function Exit({ ctx, shelterId }: { ctx: AppContext; shelterId: string }) {
  const { active, api, db, queue } = ctx
  const online = useOnline()
  const { search } = useLocation()
  const params = new URLSearchParams(search)
  const captureId = params.get('capture')
  const personId = params.get('person')
  const [people, setPeople] = useState<PersonSummary[]>([])
  const [query, setQuery] = useState('')
  const [person, setPerson] = useState<PersonView | null>(null)
  const [destination, setDestination] = useState<ExitDestination | ''>('')
  const [municipality, setMunicipality] = useState('')
  const [guardianId, setGuardianId] = useState('')
  const [guardianNames, setGuardianNames] = useState<Record<string, string>>({})
  const [reason, setReason] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [state, setState] = useState<'form' | 'working' | 'done' | 'queued'>(captureId ? 'working' : 'form')
  const [missing, setMissing] = useState(false)
  const resumed = useRef(false)

  useEffect(() => {
    if (!active) return navigate('/login', true)
    if (personId && online) return
    if (!online) return void cachedPeople(db, shelterId).then(setPeople)
    void api.listPeople(active.secret, shelterId).then(async (res) => setPeople(res.ok ? res.data.people : await cachedPeople(db, shelterId)))
  }, [online, shelterId, personId])

  useEffect(() => {
    if (!active || !personId || !online) return
    void api.getPerson(active.secret, personId).then(async (res) => {
      if (!res.ok) return setMissing(true)
      setPerson(res.data.person)
      const rows = await Promise.all(res.data.person.guardians.map(async (g) => [g.person_id, await api.getPerson(active.secret, g.person_id)] as const))
      setGuardianNames(Object.fromEntries(rows.map(([id, r]) => [id, r.ok ? nameText(displayName(r.data.person)) : t('person.no_name')])))
    })
  }, [personId, online])

  // Back from the guardian's selfie: send the exit with the capture.
  useEffect(() => {
    if (!captureId || !active || resumed.current) return
    resumed.current = true
    void (async () => {
      const draft = await readDraft<{ request: ExitRequest }>(db, captureId)
      if (!draft) return navigate(`/shelter/${shelterId}/exit`, true)
      const res = await pollCapture(() => api.postExit(active.secret, shelterId, { ...draft.request, capture_id: captureId }))
      await finishCapture(db, captureId)
      navigate(`/shelter/${shelterId}/exit?person=${draft.request.person_id}`, true)
      if (res.ok) setState('done')
      else {
        setError(res.error === 'timeout' ? 'selfie_timeout' : res.error)
        setState('form')
      }
    })()
  }, [captureId, active?.volunteer_id])

  if (!active) return null
  const secret = active.secret
  const errorText = error === 'selfie_timeout' ? t('checkin.selfie_timeout') : error ? t(exitErrorKey(error)) : null
  const isProt = isProtectionOrDc(active.global_roles)
  const request = (extra: { guardianId?: string; exceptionReason?: string } = {}) =>
    personId ? buildExit({ personId, destination, municipality, ...extra }) : null

  async function adultExit() {
    const body = request()
    if (!body) return setError('destination_required')
    await queue.enqueue(queuedExit(shelterId, active!.volunteer_id, body))
    setState(online ? 'done' : 'queued')
  }

  async function revalidate() {
    const body = request({ guardianId })
    if (!body) return setError('destination_required')
    const res = await startCapture({ db, api }, secret, 'revalidation', { shelter_id: shelterId, expected_person_id: guardianId }, `/shelter/${shelterId}/exit?person=${personId}`, { request: body })
    if (res.ok) return location.assign(res.link)
    setError(res.error)
  }

  async function exception() {
    const body = request({ exceptionReason: reason })
    if (!body) return setError('destination_required')
    if (!body.exception) return setError('invalid_reason')
    const res = await api.postExit(secret, shelterId, body)
    if (res.ok) setState('done')
    else setError(res.error)
  }

  if (state === 'working') {
    return <main class="screen"><p class="body" role="status">{t('child.exit_waiting')}</p></main>
  }
  if (state === 'done' || state === 'queued') {
    return (
      <main class="screen">
        <h1 class="title">{t(state === 'done' ? 'exit.done' : 'exit.done_queued')}</h1>
        <button type="button" class="button" onClick={() => { setState('form'); setDestination(''); setMunicipality(''); setError(null); navigate(`/shelter/${shelterId}/exit`, true) }}>{t('exit.another')}</button>
        <button type="button" class="button button-secondary" onClick={() => navigate(`/shelter/${shelterId}`)}>{t('nav.back')}</button>
      </main>
    )
  }

  // 1. Who is leaving.
  if (!personId) {
    const shown = people.filter((p) => matchesQuery(p, query))
    return (
      <>
        <div class="screen back-bar"><BackLink /></div>
        <main class="screen">
          <h1 class="title">{t('exit.pick')}</h1>
          <label class="field">
            <span class="caption">{t('exit.search')}</span>
            <input type="search" value={query} onInput={(e) => setQuery(e.currentTarget.value)} />
          </label>
          <ul class="list">
            {shown.map((p) => (
              <li key={p.id}>
                <PersonRow name={displayName(p)} level={p.level} isChild={p.is_child} onOpen={() => navigate(`/shelter/${shelterId}/exit?person=${p.id}`)} />
              </li>
            ))}
          </ul>
          {shown.length === 0 && <p class="body">{t('exit.no_people')}</p>}
        </main>
      </>
    )
  }

  const summary = people.find((p) => p.id === personId)
  const isChild = person?.is_child ?? summary?.is_child ?? false
  const header = person ? nameText(displayName(person)) : summary ? nameText(displayName(summary)) : ''
  const guardian = person?.guardians.find((g) => g.person_id === guardianId)
  const route = person ? childExitRoute(person, guardian) : 'protection_only'
  const mayRegister = canRegisterChildExit(route, active.global_roles)

  return (
    <>
      <div class="screen back-bar"><BackLink /></div>
      <main class="screen">
        <h1 class="title">{t('exit.title')}{header ? `: ${header}` : ''}</h1>
        {missing && <p class="notice notice-error" role="alert">{t('exit.not_found')}</p>}
        {errorText && <p class="notice notice-error" role="alert">{errorText}</p>}
        {error === 'destination_required' && <p class="notice notice-error" role="alert">{t('exit.destination_required')}</p>}

        <fieldset class="card fieldset">
          <legend class="body-strong">{t('exit.destination')}</legend>
          {EXIT_DESTINATIONS.map((d) => (
            <label class="check" key={d}>
              <input type="radio" name="destination" checked={destination === d} onChange={() => setDestination(d)} />
              {t(`exit.destination.${d}`)}
            </label>
          ))}
        </fieldset>
        <label class="field">
          {t('exit.municipality')}
          <input type="text" maxLength={100} value={municipality} onInput={(e) => setMunicipality(e.currentTarget.value)} />
        </label>

        {!isChild && <button type="button" class="button" onClick={() => void adultExit()}>{t('exit.submit')}</button>}

        {isChild && !online && <p class="notice" role="status">{t('child.exit_needs_signal')}</p>}
        {isChild && online && person && (
          <section class="card">
            <p class="notice notice-warning" role="status">{t('child.exit_banner')}</p>
            <label class="field">
              {t('child.exit_guardian')}
              <select value={guardianId} onChange={(e) => setGuardianId(e.currentTarget.value)}>
                <option value="" />
                {person.guardians.map((g) => <option value={g.person_id} key={g.person_id}>{guardianNames[g.person_id] ?? g.person_id} · {t(`child.link.${g.link}`)}</option>)}
              </select>
            </label>
            {person.guardians.length === 0 && <p class="caption">{t('child.exit_no_guardian')}</p>}
            {guardianId !== '' && !mayRegister && <p class="notice" role="status">{t('child.exit_protection_only')}</p>}
            <button type="button" class="button" disabled={guardianId === '' || !mayRegister} onClick={() => void revalidate()}>{t('child.exit_revalidate')}</button>
            {isProt && (
              <>
                <h2 class="subtitle">{t('child.exit_exception')}</h2>
                <p class="caption">{t('child.exit_exception_hint')}</p>
                <label class="field">
                  {t('child.exit_exception_reason')}
                  <input type="text" maxLength={500} value={reason} onInput={(e) => setReason(e.currentTarget.value)} />
                </label>
                <button type="button" class="button button-secondary" onClick={() => void exception()}>{t('child.exit_exception_go')}</button>
              </>
            )}
          </section>
        )}
      </main>
    </>
  )
}
