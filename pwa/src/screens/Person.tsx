import type { AppContext } from '../app-context'
import { t } from '../i18n'
import { BackLink } from './BackLink'

// Stage 0 stub of phase 3 (owner E, see docs/pt_br/fases/fase-3-contrato.md): replace the body.
export function Person(_props: { ctx: AppContext; personId: string }) {
  return (
    <main class="screen">
      <BackLink />
      <h1 class="title">{t('person.title')}</h1>
    </main>
  )
}
