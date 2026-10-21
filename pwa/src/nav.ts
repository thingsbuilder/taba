import { useEffect, useState } from 'preact/hooks'

/** In-app navigation: `routes.ts` reads `location.pathname`, so this only moves the URL and re-renders. */
export function navigate(path: string, replace = false): void {
  history[replace ? 'replaceState' : 'pushState'](null, '', path)
  window.dispatchEvent(new PopStateEvent('popstate'))
}

export function useLocation(): { pathname: string; search: string } {
  const [, rerender] = useState(0)
  useEffect(() => {
    const update = () => rerender((n) => n + 1)
    window.addEventListener('popstate', update)
    return () => window.removeEventListener('popstate', update)
  }, [])
  return { pathname: location.pathname, search: location.search }
}
