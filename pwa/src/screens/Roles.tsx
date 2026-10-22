import { useEffect, useState } from 'preact/hooks'
import type { MediatorRole } from '../api-phase4'
import type { AppContext } from '../app-context'
import { errorKey } from '../app-context'
import { logEvent } from '../diagnostics'
import { useOnline } from '../hooks'
import { t } from '../i18n'
import { navigate } from '../nav'
import { MEDIATOR_ROLES, canManageRoles, roleErrorKey } from '../roles'
import { BackLink } from './BackLink'

/** `/roles` (spec 2.1): a validator or the Civil Defense grants and removes `mediator` and `protection`. Needs signal. */
export function Roles({ ctx }: { ctx: AppContext }) {
  const { active, api } = ctx
  const online = useOnline()
  const [volunteerId, setVolunteerId] = useState('')
  const [busy, setBusy] = useState(false)
  const [message, setMessage] = useState<{ ok: boolean; key: string } | null>(null)
  useEffect(() => {
    if (!active) navigate('/login', true)
  }, [active])
  if (!active) return null

  async function change(role: MediatorRole, grant: boolean) {
    const id = volunteerId.trim()
    if (!active || busy) return
    if (!id) return setMessage({ ok: false, key: 'role.id_required' })
    setBusy(true)
    const res = grant ? await api.grantRole(active.secret, id, role) : await api.revokeRole(active.secret, id, role)
    setBusy(false)
    logEvent('role_change', { role, action: grant ? 'grant' : 'revoke', status: res.ok ? 200 : res.status })
    if (res.ok) setMessage({ ok: true, key: grant ? 'role.granted' : 'role.revoked' })
    else setMessage({ ok: false, key: roleErrorKey(res.status) ?? errorKey(res.error) })
  }

  const allowed = canManageRoles(active.global_roles)
  return (
    <main class="screen">
      <BackLink />
      <h1 class="title">{t('role.title')}</h1>
      {!allowed ? (
        <p class="notice notice-error" role="alert">{t('role.not_allowed')}</p>
      ) : (
        <>
          <p class="body">{t('role.intro')}</p>
          {!online && <p class="notice" role="status">{t('match.offline')}</p>}
          <label class="field">
            {t('role.volunteer_id')}
            <input type="text" value={volunteerId} autocomplete="off" autocapitalize="off" spellcheck={false} onInput={(e) => setVolunteerId(e.currentTarget.value)} />
          </label>
          <p class="caption">{t('role.volunteer_hint')}</p>
          {message && <p class={message.ok ? 'notice' : 'notice notice-error'} role={message.ok ? 'status' : 'alert'}>{t(message.key)}</p>}
          {MEDIATOR_ROLES.map((role) => (
            <section class="card" key={role}>
              <h2 class="subtitle">{t(`role.${role}`)}</h2>
              <button type="button" class="button" disabled={busy || !online} onClick={() => void change(role, true)}>{t('role.grant')}</button>
              <button type="button" class="button button-secondary" disabled={busy || !online} onClick={() => void change(role, false)}>{t('role.revoke')}</button>
            </section>
          ))}
        </>
      )}
    </main>
  )
}
