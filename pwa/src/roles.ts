import type { MediatorRole } from './api-phase4'

// Global roles granted from the app (spec 2.1): only a validator or the Civil Defense.

export const MEDIATOR_ROLES: MediatorRole[] = ['mediator', 'protection']

export const canManageRoles = (globalRoles: string[]): boolean => globalRoles.includes('validator') || globalRoles.includes('civil_defense')

/** `403` and `404` have their own texts here; the rest follow the shared error codes. */
export function roleErrorKey(status: number): string | null {
  if (status === 403) return 'role.error_forbidden'
  if (status === 404) return 'role.error_not_found'
  return null
}
