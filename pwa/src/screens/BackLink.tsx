import { backTarget } from '../back'
import { t } from '../i18n'
import { navigate } from '../nav'
import { resolveRoute } from '../routes'

/** "Back" at the top of the inner screens, to the logical parent of the current one. */
export function BackLink() {
  const to = backTarget(resolveRoute(location.pathname))
  return (
    <a
      class="back-link"
      href={to}
      onClick={(e) => {
        e.preventDefault()
        navigate(to)
      }}
    >
      {t('nav.back')}
    </a>
  )
}
