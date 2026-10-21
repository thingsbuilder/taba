import { useEffect, useState } from 'preact/hooks'
import type { AppContext } from '../app-context'
import { navigate } from '../nav'
import { t } from '../i18n'
import { canManageRoles } from '../roles'
import { updateProfile } from '../sessions'
import { BackLink } from './BackLink'
import { ProfileForm } from './ProfileForm'
import { signOut } from './signout'

/** `/profile`: edit name, phone and "show my phone". Saved on the phone at once, sent through the queue. */
export function Profile({ ctx }: { ctx: AppContext }) {
  const { active } = ctx
  const [saved, setSaved] = useState(false)
  useEffect(() => {
    if (!active) navigate('/login', true)
  }, [active])
  if (!active) return null

  async function save(p: { name: string; phone?: string; show_phone: boolean }) {
    if (!active) return
    const fields = { name: p.name, phone: p.phone ?? null, show_phone: p.show_phone }
    await updateProfile(ctx.db, active.volunteer_id, fields)
    await ctx.queue.enqueue({ method: 'PATCH', path: '/v1/me', body: fields, volunteer_id: active.volunteer_id })
    await ctx.refresh()
    setSaved(true)
  }

  return (
    <main class="screen">
      <BackLink />
      <h1 class="title">{t('profile.title')}</h1>
      <ProfileForm initial={active.profile} submitLabel={t('profile.save')} onSubmit={(p) => void save(p)} />
      {saved && <p class="notice" role="status">{t('profile.saved')}</p>}
      <p class="caption">{t('role.my_id', { id: active.volunteer_id })}</p>
      {canManageRoles(active.global_roles) && (
        <button type="button" class="button button-secondary" onClick={() => navigate('/roles')}>{t('role.open')}</button>
      )}
      <button type="button" class="button button-secondary" onClick={() => navigate('/switch-operator')}>{t('home.switch')}</button>
      <button type="button" class="button button-secondary" onClick={() => navigate('/diagnostics')}>{t('profile.diagnostics')}</button>
      <button type="button" class="button button-secondary" onClick={() => void signOut(ctx)}>{t('home.logout')}</button>
    </main>
  )
}
