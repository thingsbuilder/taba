export type Route = 'home' | 'not_found'

// Only `/` exists in phase 0; each phase adds its routes to this switch (spec 6.1).
export function resolveRoute(pathname: string): Route {
  switch (pathname) {
    case '/':
      return 'home'
    default:
      return 'not_found'
  }
}
