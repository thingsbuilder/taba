import { useEffect, useState } from 'preact/hooks'
import type { InviteInfo } from '../api'
import type { AppContext } from '../app-context'
import { useOnline } from '../hooks'
import { t } from '../i18n'

/** `/invite/:code` in Safari on an iPhone (spec 3.0.2): iOS never hands the link to the installed app,
 *  and Safari and the app do not share storage, so the code travels by copy and paste. */
export function InviteHandoff({ ctx, code }: { ctx: AppContext; code: string }) {
  const online = useOnline()
  const [info, setInfo] = useState<InviteInfo | null>(null)
  const [copied, setCopied] = useState<'ok' | 'failed' | null>(null)

  useEffect(() => {
    if (online) void ctx.api.getInvite(code).then((res) => res.ok && setInfo(res.data))
  }, [online, code])

  async function copy() {
    try {
      await navigator.clipboard.writeText(`${location.origin}/invite/${code}`)
      setCopied('ok')
    } catch {
      setCopied('failed')
    }
  }

  return (
    <main class="screen">
      <h1 class="title">{t('invite.title')}</h1>
      {info?.valid && <p class="body-strong">{info.shelter_name}</p>}
      {info && !info.valid && <p class="notice notice-error" role="alert">{t('invite.expired')}</p>}
      {(!info || info.valid) && (
        <>
          <p class="body">{t('invite.handoff_body')}</p>
          <button type="button" class="button" onClick={() => void copy()}>{t('invite.copy')}</button>
          {copied === 'ok' && <p class="notice" role="status">{t('invite.copied')}</p>}
          {copied === 'failed' && <p class="notice notice-error" role="alert">{t('invite.copy_failed')}</p>}
          <p class="body">{t('install.body')}</p>
        </>
      )}
    </main>
  )
}
