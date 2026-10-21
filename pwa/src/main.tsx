import { render } from 'preact'
import { App } from './app'
import { getStatus } from './api'
import { openTabaDb } from './db'
import { getOrCreateDeviceId } from './device'
import { loadLanguage, setLanguage } from './i18n'
import { createQueue, startAutoDrain } from './queue'
import { watchForUpdates } from './update'
import './fonts.css'
import '../../design/tokens.css'
import './app.css'

async function main() {
  const db = await openTabaDb()
  setLanguage(await loadLanguage(db, navigator.languages))
  const queue = createQueue({
    db,
    fetch: (input, init) => fetch(input, init),
    deviceId: () => getOrCreateDeviceId(db),
    getStatus,
  })
  startAutoDrain(queue, window)

  const root = document.getElementById('app')!
  const draw = (updateApply: (() => void) | null) => render(<App db={db} queue={queue} updateApply={updateApply} />, root)
  draw(null)
  watchForUpdates((apply) => draw(apply))
}

void main()
