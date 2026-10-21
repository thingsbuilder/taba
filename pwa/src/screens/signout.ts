import type { AppContext } from '../app-context'
import { navigate } from '../nav'
import { removeSession } from '../sessions'

/** Ends the server session when there is signal; the local session goes either way. */
export async function signOut(ctx: AppContext): Promise<void> {
  const { active } = ctx
  if (!active) return
  await ctx.api.deleteSession(active.secret)
  await removeSession(ctx.db, active.volunteer_id)
  await ctx.refresh()
  navigate('/')
}
