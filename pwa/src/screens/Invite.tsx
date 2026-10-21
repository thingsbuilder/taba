import { useEffect, useState } from 'preact/hooks'
import type { InviteInfo } from '../api'
import type { AppContext } from '../app-context'
import { errorKey } from '../app-context'
import { useOnline } from '../hooks'
import { t } from '../i18n'
import { BackLink } from './BackLink'
import { loginBlock, startLogin } from '../login'

/** `/invite/:code` (spec 3.3 step 3-4): what the invite is for, then the selfie that accepts it. Public. */
export function Invite({ ctx, code }: { ctx: AppContext; code: string }) {
  const online = useOnline()
  const [info, setInfo] = useState<InviteInfo | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  useEffect(() => {
    if (online) void ctx.api.getInvite(code).then((res) => (res.ok ? setInfo(res.data) : setError(res.error)))
  }, [online, code])

  async function enter() {
    setBusy(true)
    setError(null)
    const result = await startLogin({ db: ctx.db, api: ctx.api }, code)
    if (result.ok) {
      location.assign(result.link) // the journey returns to /return?capture=<id>
      return
    }
    setError(result.error)
    setBusy(false)
  }

  const reason = loginBlock(online, ctx.status)
  const blocked = reason ? t(`login.${reason}`) : null
  return (
    <main class="screen">
      <BackLink />
      <h1 class="title">{t('invite.title')}</h1>
      {!info && !error && online && <p class="body" role="status">{t('invite.loading')}</p>}
      {info && !info.valid && <p class="notice notice-error" role="alert">{t('invite.expired')}</p>}
      {info?.valid && (
        <div class="card">
          <p class="body-strong">{info.shelter_name}</p>
          <p class="body">{t(info.role === 'admin' ? 'invite.as_admin' : 'invite.as_volunteer')}</p>
          <p class="chip">{t(info.confirmed ? 'shelter.confirmed' : 'shelter.unconfirmed')}</p>
          <p class="body">{t('invite.selfie_note')}</p>
        </div>
      )}
      {blocked && <p class="notice" role="status">{blocked}</p>}
      {error && <p class="notice notice-error" role="alert">{t(errorKey(error))}</p>}
      {info?.valid && (
        <button type="button" class="button" disabled={busy || blocked !== null} onClick={() => void enter()}>
          {busy ? t('login.starting') : t('invite.accept')}
        </button>
      )}
    </main>
  )
}
