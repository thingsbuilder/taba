import { useState } from 'preact/hooks'
import type { AppContext } from '../app-context'
import { navigate } from '../nav'
import { t } from '../i18n'
import { isValidPin } from '../pin'
import { setPin } from '../sessions'

/** First login on this device: the volunteer creates the local PIN (spec 3.1.1). */
export function PinSetup({ ctx, volunteerId }: { ctx: AppContext; volunteerId: string }) {
  const [pin, setPinValue] = useState('')
  const [again, setAgain] = useState('')
  const [error, setError] = useState<string | null>(null)

  async function save(e: Event) {
    e.preventDefault()
    if (!isValidPin(pin)) return setError(t('pin.invalid'))
    if (pin !== again) return setError(t('pin.mismatch'))
    await setPin(ctx.db, volunteerId, pin)
    await ctx.refresh()
    navigate('/', true)
  }

  return (
    <main class="screen">
      <h1 class="title">{t('pin.title')}</h1>
      <p class="body">{t('pin.body')}</p>
      <form class="form" onSubmit={(e) => void save(e)}>
        <label class="field">
          {t('pin.pin')}
          <input type="password" inputMode="numeric" autocomplete="off" maxLength={6} value={pin}
            onInput={(e) => setPinValue(e.currentTarget.value)} />
        </label>
        <label class="field">
          {t('pin.confirm')}
          <input type="password" inputMode="numeric" autocomplete="off" maxLength={6} value={again}
            onInput={(e) => setAgain(e.currentTarget.value)} />
        </label>
        {error && <p class="notice notice-error" role="alert">{error}</p>}
        <button type="submit" class="button">{t('pin.save')}</button>
      </form>
    </main>
  )
}
