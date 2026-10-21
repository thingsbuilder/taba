export type RouteName =
  | 'home' | 'login' | 'return' | 'switch_operator' | 'profile'
  | 'shelter_new' | 'shelter' | 'shelter_members' | 'invite' | 'endorse' | 'inbox'
  | 'not_found'

export interface Route {
  name: RouteName
  /** `:id` of `/shelter/:id[/members]`. */
  id?: string
  /** `:code` of `/invite/:code` and `/endorse/:code`. */
  code?: string
}

const SIMPLE: Record<string, RouteName> = {
  '/': 'home',
  '/login': 'login',
  '/return': 'return',
  '/switch-operator': 'switch_operator',
  '/profile': 'profile',
  '/shelters/new': 'shelter_new',
  '/inbox': 'inbox',
}

// Each phase adds its routes here (spec 6.1).
export function resolveRoute(pathname: string): Route {
  const simple = SIMPLE[pathname]
  if (simple) return { name: simple }
  const shelter = /^\/shelter\/([^/]+)(\/members)?$/.exec(pathname)
  if (shelter) return { name: shelter[2] ? 'shelter_members' : 'shelter', id: decodeURIComponent(shelter[1]!) }
  const code = /^\/(invite|endorse)\/([^/]+)$/.exec(pathname)
  if (code) return { name: code[1] as 'invite' | 'endorse', code: decodeURIComponent(code[2]!) }
  return { name: 'not_found' }
}
