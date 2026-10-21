import { useEffect, useState } from 'preact/hooks'
import { type Api, type ApiStatus, getStatus } from './api'
import type { AppContext } from './app-context'
import type { Db } from './db'
import { logEvent } from './diagnostics'
import { isDegraded } from './degraded'
import { INSTALL_PROMPT_SNOOZE_MS, isIosOutsideStandalone, requestPersistence } from './device'
import { useLanguage, useOnline } from './hooks'
import { type Language, t } from './i18n'
import { chooseLanguage } from './language'
import { SESSIONS_CHANGED } from './events'
import { navigate, useLocation } from './nav'
import type { Queue } from './queue'
import { type Route, resolveRoute } from './routes'
import { type LocalSession, applyMe, needsPin, getActive, listValid, removeSession } from './sessions'
import { Checkin } from './screens/Checkin'
import { Diagnostics } from './screens/Diagnostics'
import { Endorse } from './screens/Endorse'
import { Exit } from './screens/Exit'
import { Household } from './screens/Household'
import { People } from './screens/People'
import { Person } from './screens/Person'
import { Privacy } from './screens/Privacy'
import { Home } from './screens/Home'
import { Inbox } from './screens/Inbox'
import { Invite } from './screens/Invite'
import { Members } from './screens/Members'
import { NewShelter } from './screens/NewShelter'
import { Shelter } from './screens/Shelter'
import { InstallPrompt } from './screens/InstallPrompt'
import { Login } from './screens/Login'
import { NotFound } from './screens/NotFound'
import { PinSetup } from './screens/PinSetup'
import { Profile } from './screens/Profile'
import { QueueAlerts } from './screens/QueueAlerts'
import { Return } from './screens/Return'
import { SwitchOperator } from './screens/SwitchOperator'

const LANGUAGES: { id: Language; label: string }[] = [
  { id: 'pt-BR', label: 'PT' },
  { id: 'en', label: 'EN' },
]

function LanguageSwitch({ db, active, queue, refresh }: { db: Db; active: LocalSession | null; queue: Queue; refresh: () => Promise<void> }) {
  const current = useLanguage()
  return (
    <div class="lang" role="group" aria-label={t('language.label')}>
      {LANGUAGES.map(({ id, label }) => (
        <button
          type="button"
          class="lang-option"
          aria-pressed={current === id}
          onClick={() => void chooseLanguage(db, queue, active, id).then(refresh)}
        >
          {label}
        </button>
      ))}
    </div>
  )
}

/** "No signal · N queued" chip: visible whenever offline or something waits in the queue (design/README.md). */
function NetworkChip({ queue }: { queue: Queue }) {
  const online = useOnline()
  const [pending, setPending] = useState(0)
  useEffect(() => {
    const refresh = () => void queue.count().then(setPending)
    refresh()
    return queue.subscribe(refresh)
  }, [queue])
  if (online && pending === 0) return null
  const text = !online
    ? pending > 0 ? t('network.offline_queue', { n: pending }) : t('network.offline')
    : t('network.queue', { n: pending })
  return <div class="chip chip-warning" role="status">{text}</div>
}

export function App({ db, api, queue, nominatim, updateApply }: { db: Db; api: Api; queue: Queue; nominatim: AppContext['nominatim']; updateApply: (() => void) | null }) {
  useLanguage() // re-render on language change
  const online = useOnline()
  const [status, setStatus] = useState<ApiStatus | null>(null)
  const [ready, setReady] = useState(false)
  const [active, setActive] = useState<LocalSession | null>(null)
  const [sessions, setSessions] = useState<LocalSession[]>([])

  const refresh = async () => {
    const now = Date.now()
    setActive(await getActive(db, now))
    setSessions(await listValid(db, now))
    setReady(true)
  }
  useEffect(() => {
    void refresh()
    window.addEventListener(SESSIONS_CHANGED, refresh)
    return () => window.removeEventListener(SESSIONS_CHANGED, refresh)
  }, [])
  // With signal, ask the server about the active session: roles, renewal, or `401` when it ended.
  useEffect(() => {
    if (!online || !active) return
    void api.getMe(active.secret).then(async (me) => {
      if (me.ok) await applyMe(db, active.volunteer_id, me.data.global_roles, Date.now(), me.data.memberships)
      else if (me.status === 401 || me.status === 403) {
        logEvent('session_expired', { status: me.status, volunteer_id: active.volunteer_id })
        await removeSession(db, active.volunteer_id)
      } else return
      await refresh()
    })
  }, [online, active?.volunteer_id])
  const iosTab = isIosOutsideStandalone(navigator)
  // Hidden until the stored "not now" of the last 7 days is read, so the prompt does not flash.
  const [installDismissed, setInstallDismissed] = useState(true)
  useEffect(() => {
    void db.get('config', 'install_prompt_dismissed_at').then((at) => setInstallDismissed(typeof at === 'number' && Date.now() - at < INSTALL_PROMPT_SNOOZE_MS))
  }, [])

  // Spec 3.0.4: ask for the status on open and whenever the connection comes back.
  useEffect(() => {
    if (online) void getStatus().then(setStatus)
  }, [online])
  // Spec 3.0.3: asked on every start; the answer is not shown.
  useEffect(() => {
    void requestPersistence(navigator.storage)
  }, [])

  const { pathname, search } = useLocation()
  const route = resolveRoute(pathname)
  const ctx: AppContext = { db, api, queue, nominatim, active, sessions, status, refresh }
  return (
    <>
      <header class="topbar">
        <a
          href="/"
          aria-label={t('nav.home')}
          onClick={(e) => {
            e.preventDefault()
            navigate('/')
          }}
        >
          <img class="brand-mark" src="/favicon.svg" alt="Taba" width="32" height="32" />
        </a>
        <LanguageSwitch db={db} active={active} queue={queue} refresh={refresh} />
      </header>
      {updateApply && (
        <button type="button" class="banner banner-action" onClick={updateApply}>
          <strong>{t('update.available')}</strong> · {t('update.action')}
        </button>
      )}
      <NetworkChip queue={queue} />
      <QueueAlerts ctx={ctx} />
      {isDegraded(status) && <div class="banner" role="status">{t('degraded.banner')}</div>}
      {iosTab && !installDismissed && <InstallPrompt onDismiss={() => { setInstallDismissed(true); void db.put('config', Date.now(), 'install_prompt_dismissed_at') }} />}
      {ready && (needsPin(active) && route.name !== 'return' && route.name !== 'diagnostics' ? <PinSetup ctx={ctx} volunteerId={active.volunteer_id} /> : <Screen route={route} ctx={ctx} search={search} />)}
    </>
  )
}

function Screen({ route, ctx, search }: { route: Route; ctx: AppContext; search: string }) {
  switch (route.name) {
    case 'home':
      return <Home ctx={ctx} />
    case 'login':
      return <Login ctx={ctx} />
    case 'return':
      return <Return ctx={ctx} search={search} />
    case 'switch_operator':
      return <SwitchOperator ctx={ctx} />
    case 'profile':
      return <Profile ctx={ctx} />
    case 'shelter_new':
      return <NewShelter ctx={ctx} />
    case 'shelter':
      return <Shelter ctx={ctx} shelterId={route.id!} key={route.id} />
    case 'shelter_members':
      return <Members ctx={ctx} shelterId={route.id!} />
    case 'invite':
      return <Invite ctx={ctx} code={route.code!} />
    case 'endorse':
      return <Endorse ctx={ctx} code={route.code!} />
    case 'inbox':
      return <Inbox ctx={ctx} />
    case 'shelter_checkin':
      return <Checkin ctx={ctx} shelterId={route.id!} />
    case 'shelter_people':
      return <People ctx={ctx} shelterId={route.id!} />
    case 'shelter_household':
      return <Household ctx={ctx} shelterId={route.id!} householdId={route.household_id!} />
    case 'shelter_exit':
      return <Exit ctx={ctx} shelterId={route.id!} />
    case 'person':
      return <Person ctx={ctx} personId={route.id!} />
    case 'privacy':
      return <Privacy />
    case 'diagnostics':
      return <Diagnostics />
    default:
      return <NotFound />
  }
}
