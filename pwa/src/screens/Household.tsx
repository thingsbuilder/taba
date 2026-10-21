import { useEffect, useState } from 'preact/hooks'
import type { Pet } from '../api-phase3'
import type { AppContext } from '../app-context'
import { errorKey } from '../app-context'
import { formatCardCode, parseCardCode } from '../card-code'
import type { CardRecord, HouseholdRecord, PersonRecord } from '../db'
import { cancelCard, createHousehold, findLocalCard, issueCard, updateHousehold } from '../households'
import { useOnline } from '../hooks'
import { t } from '../i18n'
import { navigate } from '../nav'
import { qrSvg } from '../qr'
import { BackLink } from './BackLink'
import { QrScanner } from './QrScanner'

/** `/shelter/:id/households/:household_id` (spec 3.5.1): members, head, animals and the card. `new` makes a new id. */
export function Household({ ctx, shelterId, householdId }: { ctx: AppContext; shelterId: string; householdId: string }) {
  const { db, api, queue, active } = ctx
  const online = useOnline()
  const [household, setHousehold] = useState<HouseholdRecord | null>(null)
  const [loaded, setLoaded] = useState(false)
  const [people, setPeople] = useState<PersonRecord[]>([])
  const [names, setNames] = useState<Record<string, string>>({})
  const [card, setCard] = useState<CardRecord | null>(null)
  const [svg, setSvg] = useState('')
  const [picked, setPicked] = useState<string[]>([])
  const [head, setHead] = useState('')
  const [pet, setPet] = useState({ species: '', count: '1' })
  const [scanning, setScanning] = useState(false)
  const [cancelling, setCancelling] = useState(false)
  const [typed, setTyped] = useState('')
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    if (householdId === 'new') navigate(`/shelter/${shelterId}/households/${crypto.randomUUID()}`, true)
  }, [householdId])

  async function load() {
    const record = (await db.get('households', householdId)) ?? null
    setHousehold(record)
    setPeople(await db.getAllFromIndex('people', 'by_shelter', shelterId))
    const cards = await db.getAllFromIndex('cards', 'by_household', householdId)
    setCard(cards.find((c) => c.status === 'active') ?? null)
    setLoaded(true)
  }
  useEffect(() => {
    void load()
    return queue.subscribe(() => void load())
  }, [householdId])

  // With signal, the server's view of a household that is not waiting in the queue.
  useEffect(() => {
    if (!online || !active || household?.pending) return
    void api.getHousehold(active.secret, householdId).then(async (res) => {
      if (!res.ok) return
      setNames(Object.fromEntries(res.data.members.map((m) => [m.person_id, m.name ?? ''])))
      const record: HouseholdRecord = { ...household, id: householdId, member_ids: res.data.members.map((m) => m.person_id), head_id: res.data.head_id, pets: res.data.pets, card_code: res.data.card_code, pending: false }
      await db.put('households', record)
      if (res.data.card_code) await db.put('cards', { code: res.data.card_code, household_id: householdId, status: 'active' })
      await load()
    })
  }, [online, householdId, household?.pending])

  const code = card?.code ?? household?.card_code
  useEffect(() => {
    if (code) void qrSvg(code).then(setSvg)
    else setSvg('')
  }, [code])

  if (!active) return null
  const who = { volunteer_id: active.volunteer_id, shelter_id: shelterId }
  const nameOf = (id: string) => {
    const p = people.find((x) => x.id === id)
    return names[id] || p?.name || p?.wristband || t('household.unnamed')
  }
  const candidates = people.filter((p) => !p.household_id && !household?.member_ids.includes(p.id))

  async function run(action: () => Promise<unknown>) {
    setError(null)
    try {
      await action()
      await load()
    } catch {
      setError('generic')
    }
  }

  async function openCard(text: string) {
    const parsed = parseCardCode(text)
    if (!parsed) return setError('card.invalid')
    setScanning(false)
    const local = await findLocalCard(db, parsed)
    if (online) {
      const res = await api.getCard(active!.secret, parsed)
      if (!res.ok) return setError(res.status === 404 ? 'card.not_found' : errorKey(res.error))
      if (res.data.status === 'canceled') return setError('card.canceled')
      await db.put('cards', { code: parsed, household_id: res.data.household_id, status: 'active' })
      if (!(await db.get('households', res.data.household_id))) {
        await db.put('households', { id: res.data.household_id, member_ids: res.data.members.map((m) => m.person_id), card_code: parsed })
      }
      return navigate(`/shelter/${shelterId}/households/${res.data.household_id}`)
    }
    if (!local) return setError('card.offline_unknown')
    if (local.card.status === 'canceled') return setError('card.canceled')
    navigate(`/shelter/${shelterId}/households/${local.card.household_id}`)
  }

  const scanner = (
    <section class="card">
      <h2 class="subtitle">{t('card.scan')}</h2>
      {scanning ? (
        <QrScanner onCode={(text) => void openCard(text)} onClose={() => setScanning(false)} />
      ) : (
        <button type="button" class="button button-secondary" onClick={() => setScanning(true)}>{t('card.scan')}</button>
      )}
      <label class="field">
        {t('card.type')}
        <input type="text" class="code" autoCapitalize="characters" autoComplete="off" maxLength={12} value={typed} onInput={(e) => setTyped(e.currentTarget.value)} />
      </label>
      <button type="button" class="button button-secondary" disabled={typed.trim() === ''} onClick={() => void openCard(typed)}>{t('card.go')}</button>
    </section>
  )
  const errorNote = error && <p class="notice notice-error" role="alert">{t(error.includes('.') ? error : errorKey(error))}</p>

  if (householdId === 'new' || !loaded) return <main class="screen"><BackLink /><p class="body" role="status">{t('household.loading')}</p></main>

  // ---- a household that does not exist yet: pick the members ----
  if (!household) {
    return (
      <main class="screen">
        <BackLink />
        <h1 class="title">{t('household.title')}</h1>
        <p class="body">{t('household.new_hint')}</p>
        {candidates.length === 0 && <p class="notice" role="status">{t('household.no_candidates')}</p>}
        <ul class="list">
          {candidates.map((p) => (
            <li key={p.id}>
              <label class="field">
                <input type="checkbox" checked={picked.includes(p.id)} onChange={() => setPicked(picked.includes(p.id) ? picked.filter((x) => x !== p.id) : [...picked, p.id])} />{' '}
                {nameOf(p.id)}
              </label>
            </li>
          ))}
        </ul>
        {picked.length > 1 && (
          <label class="field">
            {t('household.head')}
            <select value={head} onChange={(e) => setHead(e.currentTarget.value)}>
              <option value="">–</option>
              {picked.map((id) => <option value={id} key={id}>{nameOf(id)}</option>)}
            </select>
          </label>
        )}
        <button type="button" class="button" disabled={picked.length === 0} onClick={() => void run(() => createHousehold(db, queue, who, { id: householdId, member_ids: picked, head_id: head || (picked.length === 1 ? picked[0] : undefined) }))}>
          {t('household.create')}
        </button>
        {errorNote}
        {scanner}
      </main>
    )
  }

  const pets = household.pets ?? []
  const savePets = (next: Pet[]) => run(() => updateHousehold(db, queue, who, householdId, { pets: next }))
  return (
    <main class="screen">
      <BackLink />
      <h1 class="title">{t('household.title')}</h1>
      {household.pending && <p class="notice" role="status">{t('household.saved_here')}</p>}
      {errorNote}

      <section class="card">
        <h2 class="subtitle">{t('household.members')}</h2>
        {household.member_ids.length === 0 && <p class="body">{t('household.no_members')}</p>}
        <ul class="list">
          {household.member_ids.map((id) => (
            <li key={id}>
              <p class="body-strong">{nameOf(id)}{household.head_id === id ? ` · ${t('household.head')}` : ''}</p>
              {household.head_id !== id && (
                <button type="button" class="button button-secondary" onClick={() => void run(() => updateHousehold(db, queue, who, householdId, { head_id: id }))}>{t('household.make_head')}</button>
              )}
              <button type="button" class="button button-secondary" onClick={() => void run(() => updateHousehold(db, queue, who, householdId, { remove_member_ids: [id] }))}>{t('household.remove')}</button>
            </li>
          ))}
        </ul>
        {candidates.length > 0 && (
          <label class="field">
            {t('household.add_member')}
            <select value="" onChange={(e) => e.currentTarget.value && void run(() => updateHousehold(db, queue, who, householdId, { add_member_ids: [e.currentTarget.value] }))}>
              <option value="">{t('household.pick_person')}</option>
              {candidates.map((p) => <option value={p.id} key={p.id}>{nameOf(p.id)}</option>)}
            </select>
          </label>
        )}
      </section>

      <section class="card">
        <h2 class="subtitle">{t('household.pets')}</h2>
        <ul class="list">
          {pets.map((p, i) => (
            <li key={`${p.species}-${i}`}>
              {p.species} × {p.count}{' '}
              <button type="button" class="button button-secondary" onClick={() => void savePets(pets.filter((_, j) => j !== i))}>{t('household.remove_pet')}</button>
            </li>
          ))}
        </ul>
        <div class="inline-form">
          <input type="text" maxLength={40} aria-label={t('household.pet_species')} placeholder={t('household.pet_species')} value={pet.species} onInput={(e) => setPet({ ...pet, species: e.currentTarget.value })} />
          <input type="number" min={1} max={99} inputMode="numeric" aria-label={t('household.pet_count')} value={pet.count} onInput={(e) => setPet({ ...pet, count: e.currentTarget.value })} />
          <button
            type="button"
            class="button button-secondary"
            disabled={pet.species.trim() === '' || !(Number(pet.count) >= 1)}
            onClick={() => {
              void savePets([...pets, { species: pet.species.trim(), count: Math.floor(Number(pet.count)) }])
              setPet({ species: '', count: '1' })
            }}
          >
            {t('household.add_pet')}
          </button>
        </div>
      </section>

      <section class="card">
        <h2 class="subtitle">{t('card.title')}</h2>
        {code ? (
          <>
            <div class="qr" role="img" aria-label={t('card.qr_label')} dangerouslySetInnerHTML={{ __html: svg }} />
            <p class="body">{t('card.code')}</p>
            <p class="code">{formatCardCode(code)}</p>
            <p class="caption">{t('card.hint')}</p>
            {cancelling ? (
              <>
                <p class="notice" role="alert">{t('card.cancel_confirm')}</p>
                <button type="button" class="button" onClick={() => { setCancelling(false); void run(() => cancelCard(db, queue, who, household, code, true)) }}>{t('card.cancel_replace')}</button>
                <button type="button" class="button button-secondary" onClick={() => { setCancelling(false); void run(() => cancelCard(db, queue, who, household, code, false)) }}>{t('card.cancel_only')}</button>
              </>
            ) : (
              <button type="button" class="button button-secondary" onClick={() => setCancelling(true)}>{t('card.cancel')}</button>
            )}
          </>
        ) : (
          <>
            <p class="body">{t('card.none')}</p>
            <button type="button" class="button" onClick={() => void run(() => issueCard(db, queue, who, household))}>{t('card.issue')}</button>
          </>
        )}
      </section>
      {scanner}
    </main>
  )
}
