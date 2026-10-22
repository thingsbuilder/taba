export type RouteName =
  | 'home' | 'login' | 'return' | 'switch_operator' | 'profile'
  | 'shelter_new' | 'shelter' | 'shelter_members' | 'invite' | 'endorse' | 'inbox'
  | 'shelter_checkin' | 'shelter_people' | 'shelter_household' | 'shelter_exit' | 'shelter_delivery' | 'shelter_items' | 'shelter_review' | 'person' | 'privacy' | 'diagnostics'
  | 'rescue' | 'search' | 'roles' | 'match' | 'family'
  | 'not_found'

export interface Route {
  name: RouteName
  /** `:id` of `/shelter/:id[/...]` and `/person/:id`. */
  id?: string
  /** `:household_id` of `/shelter/:id/households/:household_id`. */
  household_id?: string
  /** `:code` of `/invite/:code`, `/endorse/:code` and the public `/f/:code` (family link). */
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
  '/privacy': 'privacy',
  '/rescue': 'rescue',
  '/search': 'search',
  '/roles': 'roles',
  '/diagnostics': 'diagnostics',
}

/** The family's page is public and stands alone: no session, PIN, top bar, queue or install prompt. */
export const usesShell = (route: Route): boolean => route.name !== 'family'

// Each phase adds its routes here (spec 6.1).
export function resolveRoute(pathname: string): Route {
  const simple = SIMPLE[pathname]
  if (simple) return { name: simple }
  const shelter = /^\/shelter\/([^/]+)(\/members)?$/.exec(pathname)
  if (shelter) return { name: shelter[2] ? 'shelter_members' : 'shelter', id: decodeURIComponent(shelter[1]!) }
  const sub = /^\/shelter\/([^/]+)\/(checkin|people|exit|delivery|items|review)$/.exec(pathname)
  if (sub) return { name: `shelter_${sub[2]}` as RouteName, id: decodeURIComponent(sub[1]!) }
  const household = /^\/shelter\/([^/]+)\/households\/([^/]+)$/.exec(pathname)
  if (household) return { name: 'shelter_household', id: decodeURIComponent(household[1]!), household_id: decodeURIComponent(household[2]!) }
  const person = /^\/person\/([^/]+)$/.exec(pathname)
  if (person) return { name: 'person', id: decodeURIComponent(person[1]!) }
  const match = /^\/matches\/([^/]+)$/.exec(pathname)
  if (match) return { name: 'match', id: decodeURIComponent(match[1]!) }
  const family = /^\/f\/([^/]+)$/.exec(pathname)
  if (family) return { name: 'family', code: decodeURIComponent(family[1]!) }
  const code = /^\/(invite|endorse)\/([^/]+)$/.exec(pathname)
  if (code) return { name: code[1] as 'invite' | 'endorse', code: decodeURIComponent(code[2]!) }
  return { name: 'not_found' }
}
