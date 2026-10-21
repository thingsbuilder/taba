import type { Route } from './routes'

/** The screen "Back" returns to: the logical parent, so it works after a deep link or a reload. */
export function backTarget(route: Route): string {
  return route.name === 'shelter_members' && route.id ? `/shelter/${route.id}` : '/'
}
