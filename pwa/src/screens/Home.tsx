import { t } from '../i18n'

export function Home() {
  return (
    <main class="screen">
      <h1 class="display">{t('home.title')}</h1>
      <p class="body">{t('home.body')}</p>
    </main>
  )
}
