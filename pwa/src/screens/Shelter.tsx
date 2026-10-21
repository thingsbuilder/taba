import { useEffect, useState } from 'preact/hooks'
import type { ApiResult, ShelterView } from '../api'
import type { AppContext } from '../app-context'
import { errorKey } from '../app-context'
import type { QueueItem, ShelterRecord } from '../db'
import { useOnline } from '../hooks'
import { t } from '../i18n'
import { navigate } from '../nav'
import { pendingViews } from '../pending'
import { roleIn } from '../sessions'
import { actionsFor, conflictOf, saveView } from '../shelters'
import { BackLink } from './BackLink'
import { EndorseRequest } from './EndorseRequest'
import { Occupancy } from './Occupancy'

/** `/shelter/:id` (design: shelter bar). Online it shows the server's view; offline, the phone's copy. */
export function Shelter({ ctx, shelterId }: { ctx: AppContext; shelterId: string }) {
  const { active, api, queue } = ctx
  const online = useOnline()
  const [view, setView] = useState<ShelterView | null>(null)
  const [record, setRecord] = useState<ShelterRecord | null>(null)
  const [items, setItems] = useState<QueueItem[]>([])
  const [error, setError] = useState<string | null>(null)
  const [reason, setReason] = useState('')
  const [capacity, setCapacity] = useState('')
  const [eventName, setEventName] = useState('')
  const [contacts, setContacts] = useState({ civil_defense_phone: '', child_protection_phone: '' })

  const refreshLocal = async () => {
    setRecord((await ctx.db.get('shelters', shelterId)) ?? null)
    setItems((await queue.list()).filter((i) => i.shelter_id === shelterId))
  }
  useEffect(() => {
    void refreshLocal()
    return queue.subscribe(() => void refreshLocal())
  }, [shelterId])
  useEffect(() => {
    if (!active) return navigate('/login', true)
    if (online) {
      void api.getShelter(active.secret, shelterId).then((res) => {
        if (!res.ok) return
        setView(res.data.shelter)
        void saveView(ctx.db, res.data.shelter)
      })
    }
  }, [online, shelterId, items.length])
  useEffect(() => {
    setCapacity(String(view?.capacity ?? record?.capacity ?? ''))
    setEventName(view?.event?.name ?? '')
    setContacts({ civil_defense_phone: view?.event?.reference_contacts.civil_defense_phone ?? '', child_protection_phone: view?.event?.reference_contacts.child_protection_phone ?? '' })
  }, [view?.capacity, view?.event?.name])
  if (!active) return null
  const secret = active.secret

  const name = view?.name ?? record?.name
  const confirmation = view?.confirmation ?? record?.confirmation ?? 'unconfirmed'
  const status = view?.status ?? record?.status
  const address = view?.address ?? record?.address
  const role = view?.role ?? roleIn(active, shelterId)
  const actions = actionsFor(role, active.global_roles, { confirmation, status })
  const held = items.find((i) => i.state === 'held' && i.error === 'shelter_nearby')
  const conflict = held ? conflictOf(held.detail) : null
  const syncing = items.some((i) => i.state === 'pending')

  async function run(call: () => Promise<ApiResult<{ shelter: ShelterView }>>) {
    setError(null)
    const res = await call()
    if (!res.ok) return setError(res.error)
    setView(res.data.shelter)
    await saveView(ctx.db, res.data.shelter)
  }

  const saveCapacity = () => {
    const value = capacity.trim() === '' ? null : Number(capacity)
    return run(() => ctx.api.patchShelter(secret, shelterId, { capacity: value }))
  }

  async function saveEvent(body: Parameters<typeof api.patchEvent>[2]) {
    if (!view?.event) return
    setError(null)
    const res = await api.patchEvent(secret, view.event.id, body)
    if (!res.ok) return setError(res.error)
    setView({ ...view, event: res.data.event })
  }

  const trimmed = (c: typeof contacts) => ({
    ...(c.civil_defense_phone.trim() ? { civil_defense_phone: c.civil_defense_phone.trim() } : {}),
    ...(c.child_protection_phone.trim() ? { child_protection_phone: c.child_protection_phone.trim() } : {}),
  })

  if (!name) {
    return (
      <main class="screen">
        <BackLink />
        <p class="body" role="status">{online ? t('shelter.loading') : t('shelter.not_cached')}</p>
        {error && <p class="notice notice-error" role="alert">{t(errorKey(error))}</p>}
      </main>
    )
  }

  const place = [address?.city, address?.state].filter(Boolean).join('/')
  return (
    <>
      <div class="screen back-bar">
        <BackLink />
      </div>
      <div class="shelter-bar">
        <img class="brand-mark" src="/favicon.svg" alt="" width="32" height="32" />
        <div class="shelter-bar-text">
          <h1 class="title">{name}</h1>
          <p class="caption">{place || t('shelter.no_address')}</p>
        </div>
        <span class={`chip chip-inline ${confirmation === 'confirmed' ? '' : 'chip-neutral'}`}>
          {t(confirmation === 'confirmed' ? 'shelter.confirmed' : 'shelter.unconfirmed')}
        </span>
      </div>
      <main class="screen">
        {status === 'closed' && <p class="notice" role="status">{t('shelter.closed')}</p>}
        {syncing && !conflict && <p class="notice" role="status">{t('shelter.syncing')}</p>}
        {conflict && (
          <section class="card" role="alert">
            <h2 class="subtitle">{t('conflict.title', { name: conflict.name, first_name: conflict.firstName })}</h2>
            <p class="body">{t('conflict.distance', { m: conflict.distanceM })}{conflict.phone ? ` · ${conflict.phone}` : ''}</p>
            <p class="body-strong">{t('conflict.other_site')}</p>
            <p class="caption">{t('conflict.hint')}</p>
            <textarea class="field-text" rows={3} maxLength={500} value={reason} onInput={(e) => setReason(e.currentTarget.value)} />
            <button type="button" class="button" disabled={reason.trim() === ''} onClick={() => void queue.release(shelterId, { other_site_reason: reason.trim() })}>
              {t('conflict.send')}
            </button>
          </section>
        )}
        {pendingViews(items, shelterId).filter((p) => p.state === 'needs_action').map((p) => (
          <p class="notice notice-error" role="alert" key={p.id}>{t(p.labelKey)}: {t(p.errorKey ?? 'error.generic')}</p>
        ))}
        {error && <p class="notice notice-error" role="alert">{t(errorKey(error))}</p>}

        {view && <Occupancy shelter={view} />}
        {role && status !== 'closed' && (
          <>
            <button type="button" class="button" onClick={() => navigate(`/shelter/${shelterId}/checkin`)}>{t('checkin.title')}</button>
            <button type="button" class="button button-secondary" onClick={() => navigate(`/shelter/${shelterId}/people`)}>{t('person.list_title')}</button>
            <button type="button" class="button button-secondary" onClick={() => navigate(`/shelter/${shelterId}/exit`)}>{t('exit.title')}</button>
          </>
        )}

        <section class="card">
          <p class="body-strong">{t('shelter.capacity', { n: view?.capacity ?? record?.capacity ?? '–' })}</p>
          {view?.event && <p class="caption">{t('shelter.event', { name: view.event.name })}</p>}
          {actions.editCapacity && online && !conflict && (
            <div class="inline-form">
              <input type="number" min={1} inputMode="numeric" aria-label={t('shelter_new.capacity')} value={capacity} onInput={(e) => setCapacity(e.currentTarget.value)} />
              <button type="button" class="button button-secondary" onClick={() => void saveCapacity()}>{t('shelter.save')}</button>
            </div>
          )}
        </section>

        {view && actions.renameEvent && view.event && online && (
          <section class="card">
            <label class="field">
              {t('event.name')}
              <input type="text" maxLength={100} value={eventName} onInput={(e) => setEventName(e.currentTarget.value)} />
            </label>
            <button type="button" class="button button-secondary" disabled={eventName.trim() === ''} onClick={() => void saveEvent({ name: eventName.trim() })}>{t('shelter.save')}</button>
          </section>
        )}
        {view && actions.editContacts && view.event && online && (
          <section class="card">
            <h2 class="subtitle">{t('event.contacts')}</h2>
            <label class="field">
              {t('event.civil_defense_phone')}
              <input type="tel" value={contacts.civil_defense_phone} onInput={(e) => setContacts({ ...contacts, civil_defense_phone: e.currentTarget.value })} />
            </label>
            <label class="field">
              {t('event.child_protection_phone')}
              <input type="tel" value={contacts.child_protection_phone} onInput={(e) => setContacts({ ...contacts, child_protection_phone: e.currentTarget.value })} />
            </label>
            <button type="button" class="button button-secondary" onClick={() => void saveEvent({ reference_contacts: trimmed(contacts) })}>{t('shelter.save')}</button>
          </section>
        )}

        {actions.members && <button type="button" class="button" onClick={() => navigate(`/shelter/${shelterId}/members`)}>{t('shelter.members')}</button>}
        {actions.requestEndorsement && online && <EndorseRequest ctx={ctx} shelterId={shelterId} />}
        {online && actions.confirm && (
          <button type="button" class="button button-secondary" onClick={() => void run(() => api.confirmShelter(secret, shelterId))}>{t('shelter.confirm')}</button>
        )}
        {online && actions.revoke && (
          <button type="button" class="button button-secondary" onClick={() => {
            const why = prompt(t('shelter.revoke_reason'))
            if (why?.trim()) void run(() => api.revokeShelter(secret, shelterId, why.trim()))
          }}>{t('shelter.revoke')}</button>
        )}
        {online && actions.close && (
          <button type="button" class="button button-secondary" onClick={() => {
            if (confirm(t('shelter.close_confirm'))) void run(() => api.closeShelter(secret, shelterId))
          }}>{t('shelter.close')}</button>
        )}
      </main>
    </>
  )
}
