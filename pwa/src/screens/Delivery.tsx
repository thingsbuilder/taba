import { useEffect, useRef, useState } from 'preact/hooks'
import type { DeliveryResponse, Items } from '../api-phase5'
import type { AppContext } from '../app-context'
import { parseCardCode } from '../card-code'
import { readDraft, startCapture } from '../captures'
import type { QueueItem } from '../db'
import {
  type CardMember,
  type DeliveryBody,
  type DeliveryDraft,
  MAX_QUANTITY,
  adminApprovalOffered,
  adminOnDevice,
  approveByAdmin,
  buildDelivery,
  deliveryErrorKey,
  enqueueDelivery,
  loadItems,
  needsOfflineApproval,
  resolveCard,
  WAITING_ERRORS,
  restoreFromDraft,
  resultText,
  selfieAllowed,
  sendState,
  sendWithCapture,
  takeDeliveryReply,
  validateDelivery,
  waitingDeliveries,
} from '../delivery'
import { isDegraded } from '../degraded'
import { useOnline } from '../hooks'
import { t } from '../i18n'
import { navigate, useLocation } from '../nav'
import { setReturnTo } from '../return-to'
import { BackLink } from './BackLink'
import { QrScanner } from './QrScanner'

/** `/shelter/:id/delivery` (spec 3.5.3): read the card, pick who takes the item, deliver. Goes through the queue, so it works offline. */
export function Delivery({ ctx, shelterId }: { ctx: AppContext; shelterId: string }) {
  const { db, api, queue, active, sessions, status } = ctx
  const online = useOnline()
  const here = `/shelter/${shelterId}/delivery`
  const captureId = new URLSearchParams(useLocation().search).get('capture')
  const withoutSelfie = !online || isDegraded(status)

  const [items, setItems] = useState<Items>({})
  const [scanning, setScanning] = useState(false)
  const [typed, setTyped] = useState('')
  const [card, setCard] = useState<{ code: string; members: CardMember[] } | null>(null)
  const [personId, setPersonId] = useState('')
  const [item, setItem] = useState('')
  const [quantity, setQuantity] = useState('1')
  const [error, setError] = useState<string | null>(null)
  const [asking, setAsking] = useState<DeliveryBody | null>(null) // high value item with no selfie possible: an admin may approve
  const [sent, setSent] = useState<{ queueId: string; reply: DeliveryResponse | null; saved: boolean } | null>(null)
  const [done, setDone] = useState<DeliveryResponse | null>(null)
  const [working, setWorking] = useState(!!captureId)
  const [waiting, setWaiting] = useState<QueueItem[]>([])
  const [waitingNames, setWaitingNames] = useState<Record<string, string>>({})
  const resumed = useRef(false)
  const cardOpened = useRef(false)
  const cardParam = new URLSearchParams(location.search).get('card')
  const discarded = useRef(false)
  const sentId = useRef<string | null>(null)

  useEffect(() => {
    if (!active) return navigate('/login', true)
    void loadItems(db, api, active.secret, shelterId, online).then(setItems)
  }, [online, shelterId])

  // The queue's view: what waits for a person here, and the answer of the delivery just sent.
  useEffect(() => {
    const load = async () => {
      const all = await queue.list()
      const mine = waitingDeliveries(all, shelterId)
      setWaiting(mine)
      const names: Record<string, string> = {}
      for (const w of mine) {
        const b = w.body as { person_id?: string }
        const p = b.person_id ? await db.get('people', b.person_id) : undefined
        names[w.id] = p?.name || p?.wristband || t('delivery.unnamed')
      }
      setWaitingNames(names)
      if (sent && !sent.reply) {
        const item = all.find((i) => i.id === sent.queueId)
        const reply = item ? null : await takeDeliveryReply(db, sent.queueId)
        const state = sendState({ item, reply, online, discarded: discarded.current })
        if (state.kind === 'done') setSent({ ...sent, reply: state.reply })
        // Stopped for a selfie, an admin or an error: leave "Sending…" and show what a person can do.
        else if (state.kind === 'waiting' || state.kind === 'canceled') setSent(null)
        else if (state.kind === 'failed') {
          setSent(null)
          setError(state.error)
        }
      }
    }
    void load()
    return queue.subscribe(() => void load())
  }, [shelterId, sent?.queueId])

  // Back from the selfie: the delivery goes out with the capture.
  useEffect(() => {
    if (!captureId || !active || resumed.current) return
    resumed.current = true
    void (async () => {
      const draft = await readDraft<DeliveryDraft>(db, captureId)
      if (!draft) return navigate(here, true)
      const res = await sendWithCapture({ db, api, queue }, active.secret, shelterId, captureId, draft)
      navigate(here, true)
      if (res.ok) setDone(res.reply)
      else {
        setError(res.error)
        // The selfie did not settle it: back to the card and the item, not to an empty screen.
        const back = await restoreFromDraft({ db, api }, active.secret, online, draft, t('delivery.unnamed'))
        if (back) {
          setCard(back.card)
          setPersonId(back.personId)
          setItem(back.item)
          setQuantity(back.quantity)
        }
      }
      setWorking(false)
    })()
  }, [captureId, active?.volunteer_id])

  // Coming from the card of a household: it is already read.
  useEffect(() => {
    if (!active || !cardParam || cardOpened.current) return
    cardOpened.current = true
    void openCard(cardParam)
  }, [active?.volunteer_id])

  if (!active) return null
  const secret = active.secret
  const admin = adminOnDevice(sessions, shelterId)
  const itemNames = Object.keys(items)
  const noList = itemNames.length === 0
  const errorNote = error && <p class="notice notice-error" role="alert">{t(error.includes('.') ? error : deliveryErrorKey(error))}</p>

  async function openCard(text: string) {
    const code = parseCardCode(text)
    if (!code) return setError('card.invalid')
    setScanning(false)
    setError(null)
    const res = await resolveCard(db, api, secret, online, code, t('delivery.unnamed'))
    if (!res.ok) return setError(res.error)
    setCard({ code, members: res.members })
    setPersonId(res.members.length === 1 ? res.members[0]!.id : '')
  }

  const request = (adminApproval = false): DeliveryBody | null => {
    const invalid = validateDelivery({ item, quantity })
    if (invalid || !card || personId === '') {
      setError(invalid ?? 'delivery.error_pick_person')
      return null
    }
    setError(null)
    return buildDelivery({ id: crypto.randomUUID(), cardCode: card.code, personId, item, quantity: Number(quantity), at: new Date(), offline: withoutSelfie, adminApproval })
  }

  async function submit(body: DeliveryBody, volunteerId = active!.volunteer_id) {
    const queued = await enqueueDelivery(queue, { volunteer_id: volunteerId, shelter_id: shelterId }, body)
    discarded.current = false
    sentId.current = queued.id
    setAsking(null)
    setSent({ queueId: queued.id, reply: null, saved: !online })
  }

  function deliver() {
    const body = request()
    if (!body) return
    // Without an admin on the device the server decides: it keeps the delivery in review (`liveness_unavailable`).
    if (admin && needsOfflineApproval(items[body.item], withoutSelfie)) return setAsking(body)
    void submit(body)
  }

  async function selfie(body: DeliveryBody, queueId?: string) {
    const draft: DeliveryDraft = { request: body, ...(queueId ? { queue_id: queueId } : {}) }
    const res = await startCapture({ db, api }, secret, 'revalidation', { shelter_id: shelterId, expected_person_id: body.person_id }, here, draft)
    if (res.ok) return location.assign(res.link)
    setError(res.error)
  }

  async function adminLogin() {
    await setReturnTo(db, here)
    navigate('/login')
  }

  function reset() {
    setCard(null)
    setPersonId('')
    setItem('')
    setQuantity('1')
    setSent(null)
    setDone(null)
    setAsking(null)
    setError(null)
  }

  if (working) return <main class="screen"><p class="body" role="status">{t('delivery.working')}</p></main>

  const result = done ?? sent?.reply ?? null
  if (result || sent) {
    const text = result ? resultText(result) : null
    return (
      <main class="screen">
        <h1 class="title">{text ? t(text.key, text.params) : t(sent!.saved ? 'delivery.saved_offline' : 'delivery.sending')}</h1>
        <button type="button" class="button" onClick={reset}>{t('delivery.another')}</button>
        <button type="button" class="button button-secondary" onClick={() => navigate(`/shelter/${shelterId}`)}>{t('nav.back')}</button>
      </main>
    )
  }

  const waitingList = waiting.length > 0 && (
    <section class="card">
      <h2 class="subtitle">{t('delivery.waiting_title')}</h2>
      {waiting.map((w) => {
        const body = w.body as DeliveryBody
        const settles = WAITING_ERRORS.has(w.error ?? '')
        const liveness = selfieAllowed(w)
        return (
          <div key={w.id}>
            <p class="body-strong">{t('delivery.waiting_line', { item: body.item, person: waitingNames[w.id] ?? '' })}</p>
            <p class="body">{t(deliveryErrorKey(w.error ?? ''))}</p>
            {settles && liveness && online && <button type="button" class="button" onClick={() => void selfie(body, w.id)}>{t('delivery.selfie_now')}</button>}
            {settles && adminApprovalOffered(w) && (admin ? (
              <button type="button" class="button button-secondary" onClick={() => void approveByAdmin(db, queue, w.id, admin)}>
                {t('delivery.admin_as', { name: admin.profile.name })}
              </button>
            ) : (
              <button type="button" class="button button-secondary" onClick={() => void adminLogin()}>{t('delivery.admin_login')}</button>
            ))}
            <button type="button" class="button button-secondary" onClick={() => {
              if (!confirm(t('delivery.cancel_confirm'))) return
              if (w.id === sentId.current) discarded.current = true
              void queue.discard(w.id)
            }}>{t('delivery.cancel')}</button>
          </div>
        )
      })}
    </section>
  )

  // 1. The card.
  if (!card) {
    return (
      <main class="screen">
        <BackLink />
        <h1 class="title">{t('delivery.title')}</h1>
        {errorNote}
        {waitingList}
        <section class="card">
          <h2 class="subtitle">{t('card.scan')}</h2>
          {scanning ? (
            <QrScanner onCode={(text) => void openCard(text)} onClose={() => setScanning(false)} />
          ) : (
            <button type="button" class="button" onClick={() => setScanning(true)}>{t('card.scan')}</button>
          )}
          <label class="field">
            {t('card.type')}
            <input type="text" class="code" autoCapitalize="characters" autoComplete="off" maxLength={12} value={typed} onInput={(e) => setTyped(e.currentTarget.value)} />
          </label>
          <button type="button" class="button button-secondary" disabled={typed.trim() === ''} onClick={() => void openCard(typed)}>{t('card.go')}</button>
        </section>
      </main>
    )
  }

  // 2. Who takes it, which item, how many.
  const person = card.members.find((m) => m.id === personId)
  return (
    <main class="screen">
      <BackLink />
      <h1 class="title">{t('delivery.title')}</h1>
      {errorNote}
      {waitingList}
      {withoutSelfie && <p class="notice" role="status">{t('delivery.offline_hint')}</p>}

      <fieldset class="card fieldset">
        <legend class="body-strong">{t('delivery.who')}</legend>
        {card.members.length === 0 && <p class="body">{t('delivery.no_members')}</p>}
        {card.members.map((m) => (
          <label class="check" key={m.id}>
            <input type="radio" name="person" checked={personId === m.id} onChange={() => setPersonId(m.id)} />
            {m.name}
          </label>
        ))}
      </fieldset>

      {noList ? (
        <label class="field">
          {t('delivery.item_free')}
          <input type="text" maxLength={100} value={item} onInput={(e) => setItem(e.currentTarget.value)} />
        </label>
      ) : (
        <label class="field">
          {t('delivery.item')}
          <select value={item} onChange={(e) => setItem(e.currentTarget.value)}>
            <option value="" />
            {itemNames.map((n) => <option value={n} key={n}>{n}</option>)}
          </select>
        </label>
      )}
      <label class="field">
        {t('delivery.quantity')}
        <input type="number" min={1} max={MAX_QUANTITY} inputMode="numeric" value={quantity} onInput={(e) => setQuantity(e.currentTarget.value)} />
      </label>

      {asking ? (
        <section class="card" role="alert">
          <p class="body">{t('delivery.offline_high')}</p>
          {admin && <button type="button" class="button" onClick={() => void submit({ ...asking, admin_approval: true }, admin.volunteer_id)}>{t('delivery.admin_as', { name: admin.profile.name })}</button>}
          <button type="button" class="button button-secondary" onClick={() => void submit(asking)}>{t('delivery.send_review')}</button>
        </section>
      ) : (
        <>
          <button type="button" class="button" disabled={!person} onClick={deliver}>{t('delivery.submit')}</button>
          {!withoutSelfie && (
            <button type="button" class="button button-secondary" disabled={!person} onClick={() => { const b = request(); if (b) void selfie(b) }}>
              {t('delivery.selfie_optional')}
            </button>
          )}
        </>
      )}
      <button type="button" class="button button-secondary" onClick={reset}>{t('delivery.other_card')}</button>
    </main>
  )
}
