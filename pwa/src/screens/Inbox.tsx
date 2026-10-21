import { useEffect, useState } from 'preact/hooks'
import type { AlertView } from '../api'
import { alertLink, alertPhones, alertTextKey } from '../alerts'
import type { AppContext } from '../app-context'
import { errorKey } from '../app-context'
import { useOnline } from '../hooks'
import { t } from '../i18n'
import { BackLink } from './BackLink'
import { navigate } from '../nav'

/** `/inbox` (spec 8.8): the alerts this volunteer receives. Needs signal. */
export function Inbox({ ctx }: { ctx: AppContext }) {
  const { active, api } = ctx
  const online = useOnline()
  const [alerts, setAlerts] = useState<AlertView[] | null>(null)
  const [error, setError] = useState<string | null>(null)

  const load = async () => {
    if (!active) return
    const res = await api.listAlerts(active.secret)
    if (res.ok) setAlerts(res.data.alerts)
    else setError(res.error)
  }
  useEffect(() => {
    if (!active) navigate('/login', true)
    else if (online) void load()
  }, [online])
  if (!active) return null

  async function resolve(id: string) {
    const res = await api.resolveAlert(active!.secret, id)
    if (res.ok) setAlerts((list) => list?.filter((a) => a.id !== id) ?? null)
    else setError(res.error)
  }

  return (
    <main class="screen">
      <BackLink />
      <h1 class="title">{t('inbox.title')}</h1>
      {!online && <p class="notice" role="status">{t('inbox.offline')}</p>}
      {error && <p class="notice notice-error" role="alert">{t(errorKey(error))}</p>}
      {alerts?.length === 0 && <p class="body">{t('inbox.empty')}</p>}
      <ul class="list">
        {alerts?.map((a) => {
          const link = alertLink(a)
          return (
            <li class="card" key={a.id}>
              <p class="body-strong">{t(alertTextKey(a.kind), { kind: a.kind })}</p>
              {alertPhones(a).map((p) => (
                <p class="body" key={p.textKey}>{t(p.textKey, { phone: p.phone })}</p>
              ))}
              <p class="caption">{new Date(a.created_at).toLocaleString(ctx.active?.profile.language)}</p>
              {link && <button type="button" class="button button-secondary" onClick={() => navigate(link)}>{t('inbox.open')}</button>}
              <button type="button" class="button button-secondary" onClick={() => void resolve(a.id)}>{t('inbox.resolve')}</button>
            </li>
          )
        })}
      </ul>
    </main>
  )
}
