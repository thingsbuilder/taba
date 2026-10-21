import { useEffect, useState } from 'preact/hooks'
import { APP_VERSION, type LogEvent, clearLog, formatLog, readLog } from '../diagnostics'
import { t } from '../i18n'
import { BackLink } from './BackLink'

/** `/diagnostics`: the local event log, newest first, to copy or clear. Needs no session. */
export function Diagnostics() {
  const [events, setEvents] = useState<LogEvent[]>([])
  const [copy, setCopy] = useState<'idle' | 'done' | 'failed'>('idle')
  const load = () => void readLog().then(setEvents)
  useEffect(load, [])

  const text = formatLog(events, { version: APP_VERSION, userAgent: navigator.userAgent, online: navigator.onLine, now: Date.now() })
  async function copyText() {
    try {
      await navigator.clipboard.writeText(text)
      setCopy('done')
    } catch {
      setCopy('failed')
    }
  }

  return (
    <main class="screen">
      <BackLink />
      <h1 class="title">{t('diagnostics.title')}</h1>
      <p class="caption">{t('diagnostics.intro')}</p>
      <button type="button" class="button" onClick={() => void copyText()}>{t('diagnostics.copy')}</button>
      <button type="button" class="button button-secondary" onClick={() => void clearLog().then(() => { setCopy('idle'); load() })}>{t('diagnostics.clear')}</button>
      {copy === 'done' && <p class="notice" role="status">{t('diagnostics.copied')}</p>}
      {copy === 'failed' && (
        <>
          <p class="notice notice-warning" role="alert">{t('diagnostics.copy_failed')}</p>
          <textarea class="code" readOnly rows={12} value={text} onFocus={(e) => e.currentTarget.select()} />
        </>
      )}
      {events.length === 0 ? (
        <p class="caption">{t('diagnostics.empty')}</p>
      ) : (
        <ul class="list">
          {[...events].reverse().map((e) => (
            <li class="code">
              {new Date(e.t).toLocaleTimeString()} {e.kind} {Object.entries(e.detail).map(([k, v]) => `${k}=${v}`).join(' ')}
            </li>
          ))}
        </ul>
      )}
    </main>
  )
}
