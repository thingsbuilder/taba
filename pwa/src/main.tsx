import { render } from 'preact'
import { App } from './app'
import { createApi, getStatus } from './api'
import { openTabaDb } from './db'
import { APP_VERSION, logEvent, setDiagnosticsDb } from './diagnostics'
import { SESSIONS_CHANGED } from './events'
import { getOrCreateDeviceId } from './device'
import { loadLanguage, setLanguage } from './i18n'
import { createNominatim } from './nominatim'
import { createQueue, startAutoDrain } from './queue'
import { applySynced } from './shelters'
import { getSession, removeSession } from './sessions'
import { watchForUpdates } from './update'
import './fonts.css'
import '../../design/tokens.css'
import './app.css'

async function main() {
  const db = await openTabaDb()
  setDiagnosticsDb(db)
  logEvent('app_open', { version: APP_VERSION, online: navigator.onLine })
  window.addEventListener('online', () => logEvent('online'))
  window.addEventListener('offline', () => logEvent('offline'))
  setLanguage(await loadLanguage(db, navigator.languages))
  const deviceId = () => getOrCreateDeviceId(db)
  const api = createApi((input, init) => fetch(input, init), deviceId)
  const queue = createQueue({
    db,
    fetch: (input, init) => fetch(input, init),
    deviceId,
    getStatus,
    // Each queued write goes out with the session of the volunteer who signed it (spec 9.2).
    secretFor: async (id) => (id ? ((await getSession(db, id, Date.now()))?.secret ?? null) : null),
    onSynced: (item, body) => applySynced(db, item, body),
    onUnauthorized: (id) => {
      if (id) void removeSession(db, id).then(() => window.dispatchEvent(new Event(SESSIONS_CHANGED)))
    },
  })
  startAutoDrain(queue, window)
  // Spec 6.1.6: addresses that waited for signal are looked up when the app opens and when the signal returns.
  const nominatim = createNominatim({ db, queue, fetch: (input, init) => fetch(input, init) })
  void nominatim.retryPending()
  window.addEventListener('online', () => void nominatim.retryPending())

  const root = document.getElementById('app')!
  const draw = (updateApply: (() => void) | null) => render(<App db={db} api={api} queue={queue} nominatim={nominatim} updateApply={updateApply} />, root)
  draw(null)
  watchForUpdates((apply) => draw(apply))
}

void main()
