import { useState } from 'preact/hooks'
import type { AppContext } from '../app-context'
import { errorKey } from '../app-context'
import { useOnline } from '../hooks'
import { t } from '../i18n'
import { loginBlock, startLogin } from '../login'
import { InviteCodeField } from './InviteCodeField'

export function Login({ ctx }: { ctx: AppContext }) {
  const online = useOnline()
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  async function enter() {
    setBusy(true)
    setError(null)
    const result = await startLogin({ db: ctx.db, api: ctx.api })
    if (result.ok) {
      location.assign(result.link) // the journey returns to /return?capture=<id> in a fresh page load
      return
    }
    setError(result.error)
    setBusy(false)
  }

  const reason = loginBlock(online, ctx.status)
  const blocked = reason ? t(`login.${reason}`) : null
  return (
    <main class="screen">
      <h1 class="title">{t('login.title')}</h1>
      <p class="body">{t('login.body')}</p>
      {blocked && <p class="notice" role="status">{blocked}</p>}
      {error && <p class="notice notice-error" role="alert">{t(errorKey(error))}</p>}
      <button type="button" class="button" disabled={busy || blocked !== null} onClick={() => void enter()}>
        {busy ? t('login.starting') : t('login.action')}
      </button>
      <InviteCodeField />
    </main>
  )
}
