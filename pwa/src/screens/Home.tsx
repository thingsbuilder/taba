import { useEffect, useState } from 'preact/hooks'
import type { AppContext } from '../app-context'
import { useOnline } from '../hooks'
import { navigate } from '../nav'
import { t } from '../i18n'
import { pendingViews } from '../pending'
import { InviteCodeField } from './InviteCodeField'
import { signOut } from './signout'
import type { QueueItem } from '../db'

export function Home({ ctx }: { ctx: AppContext }) {
  const { active } = ctx
  const online = useOnline()
  const [alerts, setAlerts] = useState<number | null>(null)
  const [items, setItems] = useState<QueueItem[]>([])
  useEffect(() => {
    const load = () => void ctx.queue.list().then(setItems)
    load()
    return ctx.queue.subscribe(load)
  }, [])
  useEffect(() => {
    if (online && active) void ctx.api.listAlerts(active.secret).then((res) => setAlerts(res.ok ? res.data.alerts.length : null))
  }, [online, active?.volunteer_id])

  if (!active) {
    return (
      <main class="screen">
        <h1 class="display">{t('home.title')}</h1>
        <p class="body">{t('home.body')}</p>
        <button type="button" class="button" onClick={() => navigate('/login')}>{t('home.login')}</button>
        <InviteCodeField />
      </main>
    )
  }
  const pending = pendingViews(items)
  return (
    <main class="screen">
      <h1 class="display">{t('home.greeting', { name: active.profile.name })}</h1>
      <h2 class="subtitle">{t('home.shelters')}</h2>
      {(active.memberships?.length ?? 0) === 0 && <p class="body">{t('home.no_shelters')}</p>}
      <ul class="list">
        {active.memberships?.map((m) => (
          <li key={m.shelter_id}>
            <button type="button" class="button button-secondary" onClick={() => navigate(`/shelter/${m.shelter_id}`)}>
              {m.name} · {t(m.confirmation === 'confirmed' ? 'shelter.confirmed' : 'shelter.unconfirmed')}
            </button>
          </li>
        ))}
      </ul>
      <InviteCodeField />
      <button type="button" class="button" onClick={() => navigate('/shelters/new')}>{t('home.new_shelter')}</button>
      <button type="button" class="button button-secondary" onClick={() => navigate('/inbox')}>
        {t('home.inbox')}{alerts ? ` (${alerts})` : ''}
      </button>
      {pending.length > 0 && (
        <section class="card">
          <h2 class="subtitle">{t('pending.title')}</h2>
          <ul class="list">
            {pending.map((p) => (
              <li key={p.id}>
                <button type="button" class="button button-secondary" disabled={!p.shelterId} onClick={() => p.shelterId && navigate(`/shelter/${p.shelterId}`)}>
                  {t(p.labelKey)}{p.errorKey ? `: ${t(p.errorKey)}` : ''}
                </button>
              </li>
            ))}
          </ul>
        </section>
      )}
      <button type="button" class="button button-secondary" onClick={() => navigate('/profile')}>{t('home.profile')}</button>
      <button type="button" class="button button-secondary" onClick={() => navigate('/switch-operator')}>{t('home.switch')}</button>
      <button type="button" class="button button-secondary" onClick={() => void signOut(ctx)}>{t('home.logout')}</button>
    </main>
  )
}
