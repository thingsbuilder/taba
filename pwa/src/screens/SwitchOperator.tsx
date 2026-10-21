import { useState } from 'preact/hooks'
import type { AppContext } from '../app-context'
import { navigate } from '../nav'
import { t } from '../i18n'
import { isValidPin } from '../pin'
import { type LocalSession, switchOperator } from '../sessions'

/** `/switch-operator`: pick a volunteer with a session on this device and confirm with the PIN. Works offline. */
export function SwitchOperator({ ctx }: { ctx: AppContext }) {
  const [chosen, setChosen] = useState<LocalSession | null>(null)
  const [pin, setPin] = useState('')
  const [message, setMessage] = useState<string | null>(null)

  async function confirm(e: Event) {
    e.preventDefault()
    if (!chosen) return
    if (!isValidPin(pin)) return setMessage(t('pin.invalid'))
    const result = await switchOperator(ctx.db, chosen.volunteer_id, pin, Date.now())
    setPin('')
    if (result.ok) {
      await ctx.refresh()
      return navigate('/', true)
    }
    if (result.reason === 'wrong_pin') return setMessage(t('switch.wrong', { n: result.remaining }))
    setMessage(result.reason === 'locked' ? t('switch.locked', { name: chosen.profile.name }) : t('switch.no_session'))
    setChosen(null)
    await ctx.refresh()
  }

  return (
    <main class="screen">
      <h1 class="title">{t('switch.title')}</h1>
      {message && <p class="notice notice-error" role="alert">{message}</p>}
      {chosen ? (
        <form class="form" onSubmit={(e) => void confirm(e)}>
          <label class="field">
            {t('switch.pin_for', { name: chosen.profile.name })}
            <input type="password" inputMode="numeric" autocomplete="off" maxLength={6} value={pin}
              onInput={(e) => setPin(e.currentTarget.value)} />
          </label>
          <button type="submit" class="button">{t('switch.confirm')}</button>
          <button type="button" class="button button-secondary" onClick={() => { setChosen(null); setPin(''); setMessage(null) }}>
            {t('switch.back')}
          </button>
        </form>
      ) : (
        <>
          <p class="body">{ctx.sessions.length > 0 ? t('switch.body') : t('switch.empty')}</p>
          <ul class="list">
            {ctx.sessions.map((s) => (
              <li>
                <button type="button" class="button button-secondary" onClick={() => { setChosen(s); setMessage(null) }}>
                  {s.profile.name}
                  {s.volunteer_id === ctx.active?.volunteer_id && <span class="chip-inline">{t('switch.active')}</span>}
                </button>
              </li>
            ))}
          </ul>
          <button type="button" class="button" onClick={() => navigate('/login')}>{t('switch.other')}</button>
        </>
      )}
    </main>
  )
}
