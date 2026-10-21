import { useEffect, useState } from 'preact/hooks'
import type { AppContext } from '../app-context'
import { errorKey } from '../app-context'
import type { ProfileInput } from '../api'
import { type LoginOutcome, finishLogin } from '../login'
import { navigate } from '../nav'
import { takeReturnTo } from '../return-to'
import { t } from '../i18n'
import { mustCreatePin } from '../sessions'
import { PinSetup } from './PinSetup'
import { ProfileForm } from './ProfileForm'

type Step =
  | { name: 'waiting' }
  | { name: 'profile' }
  | { name: 'pin'; volunteerId: string; next: string }
  | { name: 'error'; code: string }

/** `/return?capture=<id>`: where the Unico journey lands (spec 3.1, steps 3-4). */
export function Return({ ctx, search }: { ctx: AppContext; search: string }) {
  const captureId = new URLSearchParams(search).get('capture')
  const [step, setStep] = useState<Step>({ name: 'waiting' })

  async function run(profile?: ProfileInput) {
    if (!captureId) return navigate('/login', true)
    setStep({ name: 'waiting' })
    // A capture made for an invite is accepted through the invite (spec 3.3).
    const inviteCode = (await ctx.db.get('captures', captureId))?.invite_code
    const outcome: LoginOutcome = await finishLogin({ db: ctx.db, api: ctx.api }, captureId, profile, inviteCode)
    if (outcome.kind === 'profile_required') return setStep({ name: 'profile' })
    if (outcome.kind === 'timeout') return setStep({ name: 'error', code: 'timeout' })
    if (outcome.kind === 'error') return setStep({ name: 'error', code: outcome.code })
    await ctx.refresh()
    const destination = outcome.membership ? `/shelter/${outcome.membership.shelter_id}` : ((await takeReturnTo(ctx.db)) ?? '/')
    if (!mustCreatePin(outcome.session)) navigate(destination, true)
    else setStep({ name: 'pin', volunteerId: outcome.session.volunteer_id, next: destination })
  }

  useEffect(() => {
    void run()
  }, [])

  if (step.name === 'profile') {
    return (
      <main class="screen">
        <h1 class="title">{t('profile.title')}</h1>
        <p class="body">{t('profile.intro_new')}</p>
        <ProfileForm askLanguage submitLabel={t('profile.continue')} onSubmit={(p) => void run(p)} />
      </main>
    )
  }
  if (step.name === 'pin') return <PinSetup ctx={ctx} volunteerId={step.volunteerId} next={step.next} />
  if (step.name === 'error') {
    // A capture that was already used means this page was reloaded after a good login.
    const done = step.code === 'capture_consumed' && ctx.active
    return (
      <main class="screen">
        <p class="notice notice-error" role="alert">{t(step.code === 'timeout' ? 'return.timeout' : errorKey(step.code))}</p>
        <button type="button" class="button" onClick={() => navigate(done ? '/' : '/login', true)}>
          {done ? t('notfound.home') : t('return.retry')}
        </button>
      </main>
    )
  }
  return (
    <main class="screen">
      <p class="body" role="status">{t('return.waiting')}</p>
    </main>
  )
}
