import type { AppContext } from '../app-context'
import { navigate } from '../nav'
import { t } from '../i18n'
import { signOut } from './signout'

export function Home({ ctx }: { ctx: AppContext }) {
  const { active } = ctx
  if (!active) {
    return (
      <main class="screen">
        <h1 class="display">{t('home.title')}</h1>
        <p class="body">{t('home.body')}</p>
        <button type="button" class="button" onClick={() => navigate('/login')}>{t('home.login')}</button>
      </main>
    )
  }
  return (
    <main class="screen">
      <h1 class="display">{t('home.greeting', { name: active.profile.name })}</h1>
      <p class="body">{t('home.body')}</p>
      <button type="button" class="button button-secondary" onClick={() => navigate('/profile')}>{t('home.profile')}</button>
      <button type="button" class="button button-secondary" onClick={() => navigate('/switch-operator')}>{t('home.switch')}</button>
      <button type="button" class="button button-secondary" onClick={() => void signOut(ctx)}>{t('home.logout')}</button>
    </main>
  )
}
