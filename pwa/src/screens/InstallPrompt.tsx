import { t } from '../i18n'

/** iPhone in a browser tab (spec 3.0.2): an invitation to install, never a block. */
export function InstallPrompt({ onDismiss }: { onDismiss: () => void }) {
  return (
    <div class="banner" role="status">
      <p>{t('install.invite')}</p>
      <button type="button" class="button button-secondary" onClick={onDismiss}>{t('install.dismiss')}</button>
    </div>
  )
}
