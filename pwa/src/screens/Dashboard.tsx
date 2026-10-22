import { useEffect, useState } from 'preact/hooks'
import type { AlertView } from '../api'
import type { EventDashboard, EventList } from '../api-phase6'
import type { AppContext } from '../app-context'
import { errorKey } from '../app-context'
import { ANOMALY_KINDS, anomaliesOf, canSeeDashboard, dashboardErrorKey, formatDecimal, formatGeneratedAt, formatNumber, formatRatio, partialNotice, saveCsv } from '../dashboard'
import { logEvent } from '../diagnostics'
import { useOnline } from '../hooks'
import { getLanguage, t } from '../i18n'
import { navigate } from '../nav'
import { BackLink } from './BackLink'

const AGE_BANDS = ['0_11', '12_17', '18_59', '60_plus', 'unknown']
const LEVELS = ['verified', 'assisted', 'no_biometrics'] as const

function Row({ label, value }: { label: string; value: string }) {
  return (
    <li class="row">
      <span class="body">{label}</span> <span class="body-strong">{value}</span>
    </li>
  )
}

function Block({ title, children }: { title: string; children: preact.ComponentChildren }) {
  return (
    <section class="card">
      <h2 class="subtitle">{title}</h2>
      <ul class="list">{children}</ul>
    </section>
  )
}

/** `/dashboard` (spec 3.10): the Civil Defense panel, the daily CSV and the shelters to confirm. Needs signal. */
export function Dashboard({ ctx }: { ctx: AppContext }) {
  const { active, api } = ctx
  const online = useOnline()
  const [list, setList] = useState<EventList | null>(null)
  const [eventId, setEventId] = useState('')
  const [data, setData] = useState<EventDashboard | null>(null)
  const [alerts, setAlerts] = useState<AlertView[]>([])
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const allowed = !!active && canSeeDashboard(active.global_roles)
  useEffect(() => {
    if (!active) navigate('/login', true)
    else if (!allowed) navigate('/', true)
  }, [active, allowed])

  const fail = (status: number, code: string) => setError(dashboardErrorKey(status) ?? errorKey(code))

  useEffect(() => {
    if (!active || !allowed || !online) return
    setError(null)
    void api.getDashboardEvents(active.secret).then((res) => {
      logEvent('dashboard_events', { status: res.ok ? 200 : res.status })
      if (!res.ok) return fail(res.status, res.error)
      setList(res.data)
      setEventId((cur) => cur || res.data.events.find((e) => e.status === 'active')?.id || res.data.events[0]?.id || '')
    })
    void api.listAlerts(active.secret).then((res) => res.ok && setAlerts(res.data.alerts.filter((a) => ANOMALY_KINDS.includes(a.kind))))
  }, [online, active?.volunteer_id])

  useEffect(() => {
    if (!active || !allowed || !online || !eventId) return
    setData(null)
    void api.getDashboardEvent(active.secret, eventId).then((res) => {
      logEvent('dashboard_event', { status: res.ok ? 200 : res.status })
      if (res.ok) setData(res.data)
      else fail(res.status, res.error)
    })
  }, [online, eventId, active?.volunteer_id])

  if (!active || !allowed) return null

  async function downloadDaily() {
    if (!active || busy) return
    setBusy(true)
    const res = await api.getDailyCsv(active.secret, eventId)
    const saved = await saveCsv(res, `daily-${eventId}.csv`)
    setBusy(false)
    logEvent('dashboard_daily_csv', { status: res.status })
    if (!saved) setError(dashboardErrorKey(res.status) ?? 'error.generic')
  }

  const events = list?.events ?? null
  const summary = events?.find((e) => e.id === eventId)
  const toConfirm = data?.unconfirmed_shelters ?? summary?.unconfirmed_shelters ?? []
  const anomalies = anomaliesOf(alerts, eventId)
  const totals = data?.totals
  const pct = (n: number) => formatRatio(totals && totals.unique_people > 0 ? n / totals.unique_people : null)

  return (
    <main class="screen">
      <BackLink />
      <h1 class="title">{t('dashboard.title')}</h1>
      <p class="notice" role="note">{t('dashboard.notice')}</p>
      {!online && <p class="notice notice-error" role="alert">{t('dashboard.offline')}</p>}
      {online && error && <p class="notice notice-error" role="alert">{t(error)}</p>}
      {online && !error && !events && <p class="body" role="status">{t('dashboard.loading')}</p>}
      {online && events && events.length === 0 && <p class="body">{t('dashboard.no_events')}</p>}
      {online && list && list.complete === false && <p class="notice notice-error" role="alert">{t('dashboard.list_partial')}</p>}
      {online && events && events.length > 0 && (
        <>
          <label class="field">
            {t('dashboard.event')}
            <select value={eventId} onChange={(e) => setEventId(e.currentTarget.value)}>
              {events.map((e) => <option key={e.id} value={e.id}>{e.name} · {t(`dashboard.status.${e.status}`)}</option>)}
            </select>
          </label>

          <section class="card">
            <h2 class="subtitle">{t('dashboard.to_confirm')}</h2>
            {toConfirm.length === 0 && <p class="body">{t('dashboard.to_confirm_none')}</p>}
            <ul class="list">
              {toConfirm.map((s) => (
                <li key={s.id}>
                  <button type="button" class="button button-secondary" onClick={() => navigate(`/shelter/${s.id}`)}>
                    {s.name} · {[s.city, s.state].filter(Boolean).join('/') || t('shelter.no_address')} · {new Date(s.created_at).toLocaleDateString(getLanguage())}
                    {s.flagged ? ` · ${t('dashboard.flagged')}` : ''}
                  </button>
                  <p class="caption">
                    {[s.created_by_name, `${s.lat.toFixed(4)}, ${s.lon.toFixed(4)}`].filter(Boolean).join(' · ')}
                  </p>
                </li>
              ))}
            </ul>
          </section>

          {anomalies.length > 0 && (
            <section class="card">
              <h2 class="subtitle">{t('dashboard.anomalies')}</h2>
              <ul class="list">
                {anomalies.map((a) => <li class="body" key={a.id}>{t(`dashboard.anomaly.${a.kind}`)}</li>)}
              </ul>
            </section>
          )}

          {!data && !error && <p class="body" role="status">{t('dashboard.loading')}</p>}
          {data && totals && (
            <>
              {data.generated_at && <p class="caption">{t('dashboard.updated_at', { time: formatGeneratedAt(data.generated_at) })}</p>}
              {partialNotice(data) && <p class="notice notice-error" role="alert">{partialNotice(data)}</p>}
              <button type="button" class="button" disabled={busy} onClick={() => void downloadDaily()}>{t('dashboard.daily_csv')}</button>

              <Block title={t('dashboard.people')}>
                <Row label={t('dashboard.unique_people')} value={formatNumber(totals.unique_people)} />
                {LEVELS.map((l) => <Row key={l} label={t(`dashboard.level.${l}`)} value={`${formatNumber(totals.by_level[l])} (${pct(totals.by_level[l])})`} />)}
              </Block>
              <Block title={t('dashboard.age_bands')}>
                {AGE_BANDS.map((b) => <Row key={b} label={t(`dashboard.age.${b}`)} value={formatNumber(totals.age_bands[b] ?? 0)} />)}
              </Block>
              <Block title={t('dashboard.groups')}>
                <Row label={t('dashboard.pregnant')} value={formatNumber(totals.pregnant)} />
                <Row label={t('dashboard.lactating')} value={formatNumber(totals.lactating)} />
                {Object.entries(totals.disability).map(([k, n]) => <Row key={k} label={`${t('checkin.disability_title')}: ${t(`checkin.disability.${k}`)}`} value={formatNumber(n)} />)}
                <Row label={t('dashboard.children_separated')} value={formatNumber(totals.children_separated)} />
                <Row label={t('dashboard.children_unaccompanied')} value={formatNumber(totals.children_unaccompanied)} />
                <Row label={t('dashboard.indigenous')} value={formatNumber(totals.indigenous)} />
                <Row label={t('dashboard.migrant')} value={formatNumber(totals.migrant)} />
              </Block>
              <Block title={t('dashboard.households_pets')}>
                <Row label={t('dashboard.households')} value={formatNumber(totals.households)} />
                <Row label={t('dashboard.pets')} value={formatNumber(totals.pets)} />
              </Block>
              <Block title={t('dashboard.movement')}>
                <Row label={t('dashboard.last_24h')} value={formatNumber(data.movement.last_24h)} />
                <Row label={t('dashboard.last_7d')} value={formatNumber(data.movement.last_7d)} />
                {Object.entries(data.exits_by_destination).map(([k, n]) => <Row key={k} label={`${t('dashboard.exits')}: ${t(`exit.destination.${k}`)}`} value={formatNumber(n)} />)}
              </Block>
              <Block title={t('dashboard.operation')}>
                <Row label={t('dashboard.rescues')} value={formatNumber(data.rescues)} />
                <Row label={t('dashboard.open_alerts')} value={formatNumber(data.open_alerts)} />
                <Row label={t('dashboard.matches_central')} value={formatNumber(data.open_matches.central)} />
                <Row label={t('dashboard.matches_protection')} value={formatNumber(data.open_matches.protection)} />
              </Block>
              <Block title={t('dashboard.pilot')}>
                <Row label={t('dashboard.selfie_refusal')} value={formatRatio(data.pilot.selfie_refusal_rate)} />
                <Row label={t('dashboard.not_the_person')} value={formatNumber(data.pilot.not_the_person)} />
                <Row label={t('dashboard.other_site_reasons')} value={formatNumber(data.pilot.other_site_reasons)} />
                <Row label={t('dashboard.unico_per_person')} value={formatDecimal(data.pilot.unico_processes_per_person)} />
              </Block>

              <section class="card">
                <h2 class="subtitle">{t('dashboard.shelters')}</h2>
                {data.shelters.length === 0 && <p class="body">{t('dashboard.shelters_none')}</p>}
                <ul class="list">
                  {data.shelters.map((s) => (
                    <li key={s.id}>
                      <button type="button" class="button button-secondary" onClick={() => navigate(`/shelter/${s.id}`)}>
                        {s.name} · {t(s.confirmation === 'confirmed' ? 'shelter.confirmed' : 'shelter.unconfirmed')}
                        {s.flagged ? ` · ${t('dashboard.flagged')}` : ''}
                      </button>
                      <p class="caption">
                        {t('dashboard.capacity')}: {s.capacity === null ? '–' : formatNumber(s.capacity)} · {t('dashboard.occupancy')}: {formatNumber(s.occupancy)} · {t('dashboard.vacancies')}: {s.vacancies === null ? '–' : formatNumber(s.vacancies)}
                      </p>
                    </li>
                  ))}
                </ul>
              </section>
            </>
          )}
        </>
      )}
    </main>
  )
}
