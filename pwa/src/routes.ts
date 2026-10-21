export type Route = 'home' | 'login' | 'return' | 'switch_operator' | 'profile' | 'not_found'

// Each phase adds its routes to this switch (spec 6.1).
export function resolveRoute(pathname: string): Route {
  switch (pathname) {
    case '/':
      return 'home'
    case '/login':
      return 'login'
    case '/return':
      return 'return'
    case '/switch-operator':
      return 'switch_operator'
    case '/profile':
      return 'profile'
    default:
      return 'not_found'
  }
}
