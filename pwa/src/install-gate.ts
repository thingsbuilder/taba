import type { Route } from './routes'

/** What an iPhone outside the installed app (spec 3.0.2) may show: only an invite link gets more than the install screen. */
export function gateFor(route: Route, blocked: boolean): 'open' | 'install' | 'invite_handoff' {
  if (!blocked) return 'open'
  return route.name === 'invite' && route.code ? 'invite_handoff' : 'install'
}
