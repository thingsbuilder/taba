import { useEffect, useState } from 'preact/hooks'
import type { InviteView, MemberView } from '../api'
import type { AppContext } from '../app-context'
import { errorKey } from '../app-context'
import { useOnline } from '../hooks'
import { t } from '../i18n'
import { navigate } from '../nav'
import { roleIn } from '../sessions'
import { QrBlock } from './QrBlock'

type Invites = { invites: InviteView[]; joined_by_link: { volunteer_id: string; name: string; invite_code: string; joined_at: string }[] }

/** `/shelter/:id/members` (spec 3.3, 4.4): members, roles and invites. Needs signal. */
export function Members({ ctx, shelterId }: { ctx: AppContext; shelterId: string }) {
  const { active, api } = ctx
  const online = useOnline()
  const [members, setMembers] = useState<MemberView[]>([])
  const [invites, setInvites] = useState<Invites | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [kind, setKind] = useState<'qr' | 'link'>('qr')
  const [role, setRole] = useState<'volunteer' | 'admin'>('volunteer')
  const [created, setCreated] = useState<{ url: string; expires_at: string } | null>(null)

  const myRole = active ? roleIn(active, shelterId) : null
  const isAdmin = myRole === 'admin'
  const isDc = active?.global_roles.includes('civil_defense') ?? false

  async function load() {
    if (!active) return
    const res = await api.listMembers(active.secret, shelterId)
    if (res.ok) setMembers(res.data.members)
    else setError(res.error)
    if (isAdmin) {
      const inv = await api.listInvites(active.secret, shelterId)
      if (inv.ok) setInvites(inv.data)
    }
  }
  useEffect(() => {
    if (!active) navigate('/login', true)
    else if (online) void load()
  }, [online, shelterId])
  if (!active) return null
  const secret = active.secret

  async function act(call: () => Promise<{ ok: true } | { ok: false; error: string }>, after?: () => void) {
    setError(null)
    const res = await call()
    if (!res.ok) return setError(res.error)
    after?.()
    await load()
  }

  async function invite(e: Event) {
    e.preventDefault()
    setError(null)
    const res = await api.createInvite(secret, shelterId, { kind, role: kind === 'link' ? 'volunteer' : role })
    if (!res.ok) return setError(res.error)
    setCreated(res.data)
    await load()
  }

  return (
    <main class="screen">
      <h1 class="title">{t('members.title')}</h1>
      <button type="button" class="button button-secondary" onClick={() => navigate(`/shelter/${shelterId}`)}>{t('members.back')}</button>
      {!online && <p class="notice" role="status">{t('members.offline')}</p>}
      {error && <p class="notice notice-error" role="alert">{t(errorKey(error))}</p>}

      <ul class="list">
        {members.map((m) => (
          <li class="card" key={m.volunteer_id}>
            <p class="body-strong">{m.name}{m.volunteer_id === active.volunteer_id ? ` (${t('members.you')})` : ''}</p>
            <p class="caption">{t(m.role === 'admin' ? 'members.admin' : 'members.volunteer')}{m.phone ? ` · ${m.phone}` : ''}</p>
            {isAdmin && m.role === 'volunteer' && (
              <>
                <button type="button" class="button button-secondary" onClick={() => void act(() => api.promoteMember(secret, shelterId, m.volunteer_id))}>{t('members.promote')}</button>
                <button type="button" class="button button-secondary" onClick={() => void act(() => api.removeMember(secret, shelterId, m.volunteer_id))}>{t('members.remove')}</button>
              </>
            )}
            {isDc && m.role === 'admin' && m.volunteer_id !== active.volunteer_id && (
              <button type="button" class="button button-secondary" onClick={() => void act(() => api.removeMember(secret, shelterId, m.volunteer_id))}>{t('members.remove')}</button>
            )}
          </li>
        ))}
      </ul>

      {myRole && (
        <button type="button" class="button button-secondary" onClick={() => {
          if (confirm(t('members.leave_confirm'))) void act(() => api.leaveShelter(secret, shelterId), () => navigate('/'))
        }}>{t('members.leave')}</button>
      )}

      {isAdmin && (
        <>
          <h2 class="subtitle">{t('invites.title')}</h2>
          <form class="form" onSubmit={(e) => void invite(e)}>
            <label class="field">
              {t('invites.kind')}
              <select value={kind} onChange={(e) => { const k = e.currentTarget.value as 'qr' | 'link'; setKind(k); if (k === 'link') setRole('volunteer') }}>
                <option value="qr">{t('invites.kind_qr')}</option>
                <option value="link">{t('invites.kind_link')}</option>
              </select>
            </label>
            <label class="field">
              {t('invites.role')}
              <select value={role} disabled={kind === 'link'} onChange={(e) => setRole(e.currentTarget.value as 'volunteer' | 'admin')}>
                <option value="volunteer">{t('members.volunteer')}</option>
                <option value="admin">{t('members.admin')}</option>
              </select>
            </label>
            {kind === 'link' && <p class="caption">{t('invites.link_note')}</p>}
            <button type="submit" class="button" disabled={!online}>{t('invites.create')}</button>
          </form>
          {created && (
            <QrBlock url={created.url} label={t('invites.qr_label')}>
              <p class="caption">{t('invites.expires', { at: new Date(created.expires_at).toLocaleString(active.profile.language) })}</p>
            </QrBlock>
          )}
          {invites && invites.invites.length > 0 && (
            <>
              <h3 class="subtitle">{t('invites.active')}</h3>
              <ul class="list">
                {invites.invites.map((i) => (
                  <li class="card" key={i.code}>
                    <p class="body-strong">{t(i.kind === 'qr' ? 'invites.kind_qr' : 'invites.kind_link')} · {t(i.role === 'admin' ? 'members.admin' : 'members.volunteer')}</p>
                    <p class="caption">{t('invites.expires', { at: new Date(i.expires_at).toLocaleString(active.profile.language) })}</p>
                    <button type="button" class="button button-secondary" onClick={() => void act(() => api.cancelInvite(secret, i.code))}>{t('invites.cancel')}</button>
                  </li>
                ))}
              </ul>
            </>
          )}
          {invites && invites.joined_by_link.length > 0 && (
            <>
              <h3 class="subtitle">{t('invites.joined_by_link')}</h3>
              <ul class="list">
                {invites.joined_by_link.map((j) => <li class="card" key={j.volunteer_id}>{j.name}</li>)}
              </ul>
            </>
          )}
        </>
      )}
    </main>
  )
}
