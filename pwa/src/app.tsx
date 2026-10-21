import { useEffect, useState } from 'preact/hooks'
import { type ApiStatus, getStatus } from './api'
import type { Db } from './db'
import { isIosOutsideStandalone, requestPersistence } from './device'
import { useLanguage, useOnline } from './hooks'
import { type Language, saveLanguage, setLanguage, t } from './i18n'
import type { Queue } from './queue'
import { resolveRoute } from './routes'
import { Home } from './screens/Home'
import { InstallIos } from './screens/InstallIos'
import { NotFound } from './screens/NotFound'

const LANGUAGES: { id: Language; label: string }[] = [
  { id: 'pt-BR', label: 'PT' },
  { id: 'en', label: 'EN' },
]

function LanguageSwitch({ db }: { db: Db }) {
  const current = useLanguage()
  return (
    <div class="lang" role="group" aria-label={t('language.label')}>
      {LANGUAGES.map(({ id, label }) => (
        <button
          type="button"
          class="lang-option"
          aria-pressed={current === id}
          onClick={() => {
            setLanguage(id)
            void saveLanguage(db, id)
          }}
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

export function App({ db, queue, updateApply }: { db: Db; queue: Queue; updateApply: (() => void) | null }) {
  useLanguage() // re-render on language change
  const online = useOnline()
  const [status, setStatus] = useState<ApiStatus | null>(null)
  const [persisted, setPersisted] = useState(true)
  const blocked = isIosOutsideStandalone(navigator)

  // Spec 3.0.4: ask for the status on open and whenever the connection comes back.
  useEffect(() => {
    if (online) void getStatus().then(setStatus)
  }, [online])
  // Spec 3.0.3: only once the app is installed (on iOS, past the install screen).
  useEffect(() => {
    if (!blocked) void requestPersistence(navigator.storage).then(setPersisted)
  }, [blocked])

  const route = resolveRoute(location.pathname)
  return (
    <>
      <header class="topbar">
        <img class="brand-mark" src="/favicon.svg" alt="Taba" width="32" height="32" />
        <LanguageSwitch db={db} />
      </header>
      {updateApply && (
        <button type="button" class="banner banner-action" onClick={updateApply}>
          <strong>{t('update.available')}</strong> · {t('update.action')}
        </button>
      )}
      {!blocked && <NetworkChip queue={queue} />}
      {!blocked && status?.unico === 'degraded' && <div class="banner" role="status">{t('degraded.banner')}</div>}
      {!blocked && !persisted && <div class="banner" role="alert">{t('persist.warning')}</div>}
      {blocked ? <InstallIos /> : route === 'home' ? <Home /> : <NotFound />}
    </>
  )
}
