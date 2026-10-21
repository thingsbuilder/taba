import { t } from '../i18n'

export function NotFound() {
  return (
    <main class="screen">
      <h1 class="title">{t('notfound.title')}</h1>
      <a class="button" href="/">{t('notfound.home')}</a>
    </main>
  )
}
