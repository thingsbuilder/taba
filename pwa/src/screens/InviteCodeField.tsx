import { useState } from 'preact/hooks'
import { extractInviteCode } from '../invite-code'
import { t } from '../i18n'
import { navigate } from '../nav'

/** "I have an invite": a pasted URL or code opens the invite page inside the installed app. */
export function InviteCodeField() {
  const [value, setValue] = useState('')
  const [invalid, setInvalid] = useState(false)

  function go(e: Event) {
    e.preventDefault()
    const code = extractInviteCode(value)
    if (!code) return setInvalid(true)
    navigate(`/invite/${code}`)
  }

  return (
    <form class="card form" onSubmit={go}>
      <label class="field">
        {t('invite.have_code')}
        <input type="text" inputMode="url" autocapitalize="off" autocomplete="off" placeholder={t('invite.code_placeholder')} value={value} onInput={(e) => { setValue(e.currentTarget.value); setInvalid(false) }} />
      </label>
      {invalid && <p class="notice notice-error" role="alert">{t('invite.code_invalid')}</p>}
      <button type="submit" class="button button-secondary" disabled={value.trim() === ''}>{t('invite.code_go')}</button>
    </form>
  )
}
