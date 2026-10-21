import type { ExitDestination, ExitRequest, Guardian } from './api-phase3'
import type { ChildStatus } from './api-phase3'
import { checkinErrorKey } from './checkin'
import type { NewQueueItem } from './queue'

// Exit (spec 3.8, 4.2): an adult leaves with a destination; a child leaves with the guardian's selfie, or
// by exception through `protection` / the Civil Defense. The server decides; the screen only explains it first.

export const EXIT_DESTINATIONS: ExitDestination[] = ['own_home', 'relatives_or_friends', 'rental_aid', 'other_shelter', 'other', 'unknown']

/** `protection` and the Civil Defense hold the same powers over children's exits in this phase (doc da fase 3). */
export const isProtectionOrDc = (globalRoles: string[]): boolean => globalRoles.includes('protection') || globalRoles.includes('civil_defense')

export type ChildExitRoute = 'with_guardian' | 'protection_only'

/** Matrix 2.3: any volunteer for a `with_guardian` child and a `document_checked` guardian; the rest is protection only. */
export function childExitRoute(child: { child_status?: ChildStatus }, guardian: Pick<Guardian, 'link'> | undefined): ChildExitRoute {
  return child.child_status === 'with_guardian' && guardian?.link === 'document_checked' ? 'with_guardian' : 'protection_only'
}

export const canRegisterChildExit = (route: ChildExitRoute, globalRoles: string[]): boolean =>
  route === 'with_guardian' || isProtectionOrDc(globalRoles)

export interface ExitInput {
  personId: string
  destination: ExitDestination | ''
  municipality: string
  guardianId?: string
  captureId?: string
  exceptionReason?: string
}

/** Null while the destination is missing: it is required for everyone (criterion 37). */
export function buildExit(i: ExitInput): ExitRequest | null {
  if (i.destination === '') return null
  const municipality = i.municipality.trim()
  const reason = i.exceptionReason?.trim()
  return {
    person_id: i.personId,
    exit_destination: i.destination,
    ...(municipality ? { destination_municipality: municipality } : {}),
    ...(i.guardianId ? { guardian_id: i.guardianId } : {}),
    ...(i.captureId ? { capture_id: i.captureId } : {}),
    ...(reason ? { exception: { reason } } : {}),
  }
}

/** An adult's exit works without signal: it goes through the queue (spec 9.1). */
export const queuedExit = (shelterId: string, volunteerId: string | null, body: ExitRequest): NewQueueItem => ({
  method: 'POST',
  path: `/v1/shelters/${shelterId}/exits`,
  body,
  volunteer_id: volunteerId,
  shelter_id: shelterId,
})

const OWN_ERRORS = new Set(['protection_role_required', 'revalidation_failed', 'guardian_not_verified'])

/** i18n key for a server error of the exit and child screens. */
export const exitErrorKey = (code: string): string => {
  if (code === 'destination_required') return 'exit.destination_required'
  return OWN_ERRORS.has(code) ? `child.error.${code}` : checkinErrorKey(code)
}
