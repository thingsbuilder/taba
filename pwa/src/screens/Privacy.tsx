import { t } from '../i18n'
import { BackLink } from './BackLink'

// Stage 0 stub of phase 3 (owner E, see docs/pt_br/fases/fase-3-contrato.md): the full text of spec 10.1.3.
export function Privacy() {
  return (
    <main class="screen">
      <BackLink />
      <h1 class="title">{t('consent.privacy_title')}</h1>
    </main>
  )
}
