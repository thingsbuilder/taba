import type { Route } from './routes'

/** The screen "Back" returns to: the logical parent, so it works after a deep link or a reload. */
export function backTarget(route: Route): string {
  switch (route.name) {
    case 'shelter_members':
    case 'shelter_checkin':
    case 'shelter_people':
    case 'shelter_household':
    case 'shelter_exit':
      return route.id ? `/shelter/${route.id}` : '/'
    case 'diagnostics':
      return '/profile'
    default:
      return '/'
  }
}
