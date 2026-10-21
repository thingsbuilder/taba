import type { Db } from './db'
import type { Membership, SessionResponse, VolunteerView } from './api'
import { type Language } from './i18n'
import { type PinRecord, createPinRecord, verifyPin } from './pin'

// Sessions of every volunteer who logged in on this device (spec 3.1.1, 6.1.1):
// `config.sessions` (by volunteer id) and `config.active_volunteer_id`.

export const MAX_FAILED_PINS = 5
const DAY_MS = 24 * 3600_000

export interface Profile {
  name: string
  phone: string | null
  show_phone: boolean
  language: Language
}

export interface LocalSession extends Partial<PinRecord> {
  volunteer_id: string
  secret: string
  expires_at: number // ms since epoch
  ttl_ms: number // lifetime the server gave at login, to follow its renewal
  profile: Profile
  global_roles: string[]
  /** Shelters the volunteer belongs to, from the last `GET /v1/me` (spec 8.1). */
  memberships?: Membership[]
  failed_pins: number
}

type Sessions = Record<string, LocalSession>

export const profileOf = (v: VolunteerView): Profile => ({
  name: v.name,
  phone: v.phone,
  show_phone: v.show_phone,
  language: v.language,
})

async function readAll(db: Db): Promise<Sessions> {
  return ((await db.get('config', 'sessions')) as Sessions | undefined) ?? {}
}

const writeAll = (db: Db, sessions: Sessions) => db.put('config', sessions, 'sessions')

export async function saveSession(db: Db, session: LocalSession): Promise<void> {
  await writeAll(db, { ...(await readAll(db)), [session.volunteer_id]: session })
}

export async function removeSession(db: Db, volunteerId: string): Promise<void> {
  const { [volunteerId]: _gone, ...rest } = await readAll(db)
  await writeAll(db, rest)
  if ((await db.get('config', 'active_volunteer_id')) === volunteerId) await db.delete('config', 'active_volunteer_id')
}

export async function setActive(db: Db, volunteerId: string): Promise<void> {
  await db.put('config', volunteerId, 'active_volunteer_id')
}

export async function getSession(db: Db, volunteerId: string, now: number): Promise<LocalSession | null> {
  const session = (await readAll(db))[volunteerId]
  return session && session.expires_at > now ? session : null
}

/** The operator who signs new writes; null when nobody is logged in or the session ran out. */
export async function getActive(db: Db, now: number): Promise<LocalSession | null> {
  const id = await db.get('config', 'active_volunteer_id')
  return typeof id === 'string' ? getSession(db, id, now) : null
}

/** Volunteers that "switch operator" lists: those with a session still valid on this device. */
export async function listValid(db: Db, now: number): Promise<LocalSession[]> {
  return Object.values(await readAll(db)).filter((s) => s.expires_at > now)
}

/** Stores the session the server just issued and makes that volunteer the active operator.
 *  A PIN already created on this device is kept. */
export async function addLoginSession(db: Db, response: SessionResponse, now: number): Promise<LocalSession> {
  const previous = (await readAll(db))[response.volunteer.id]
  const expiresAt = Date.parse(response.expires_at)
  const session: LocalSession = {
    volunteer_id: response.volunteer.id,
    secret: response.session,
    expires_at: expiresAt,
    ttl_ms: expiresAt - now,
    profile: profileOf(response.volunteer),
    global_roles: previous?.global_roles ?? [],
    memberships: previous?.memberships,
    pin_hash: previous?.pin_hash,
    pin_salt: previous?.pin_salt,
    failed_pins: 0,
  }
  await saveSession(db, session)
  await setActive(db, session.volunteer_id)
  return session
}

export const hasPin = (session: LocalSession): boolean => session.pin_hash !== undefined && session.pin_salt !== undefined

// ponytail: the required PIN (spec 3.1.1) is off at the owner's request for the first pilot: "Sign out"
// erases the session together with its PIN, so every new selfie login asked for the PIN again.
// To be reviewed in phase 6 (docs/pt_br/fases/fase-6-painel-e-lgpd.md). The PIN code stays; only the
// obligation is off, and nothing else creates a PIN while it is.
export const PIN_REQUIRED = false

/** Whether this login must create the PIN before anything else. */
export const mustCreatePin = (session: LocalSession, required = PIN_REQUIRED): boolean => required && !hasPin(session)

/** A logged-in operator who has to create the PIN (also after the app was closed on that screen). */
export const needsPin = (session: LocalSession | null, required = PIN_REQUIRED): session is LocalSession =>
  session !== null && mustCreatePin(session, required)

export async function setPin(db: Db, volunteerId: string, pin: string): Promise<void> {
  const session = (await readAll(db))[volunteerId]
  if (session) await saveSession(db, { ...session, ...(await createPinRecord(pin)), failed_pins: 0 })
}

export type SwitchResult =
  | { ok: true }
  | { ok: false; reason: 'no_session' | 'no_pin' }
  | { ok: false; reason: 'wrong_pin'; remaining: number }
  | { ok: false; reason: 'locked' }

/** Spec 3.1.1: works offline; the fifth wrong PIN in a row erases that volunteer's session. */
export async function switchOperator(db: Db, volunteerId: string, pin: string, now: number): Promise<SwitchResult> {
  const session = await getSession(db, volunteerId, now)
  if (!session) return { ok: false, reason: 'no_session' }
  if (!session.pin_hash || !session.pin_salt) return { ok: false, reason: 'no_pin' }

  // Counted before the check, so closing the app mid-attempt does not give a free guess.
  const failed = session.failed_pins + 1
  await saveSession(db, { ...session, failed_pins: failed })
  if (await verifyPin(pin, { pin_hash: session.pin_hash, pin_salt: session.pin_salt })) {
    await saveSession(db, { ...session, failed_pins: 0 })
    await setActive(db, volunteerId)
    return { ok: true }
  }
  if (failed >= MAX_FAILED_PINS) {
    await removeSession(db, volunteerId)
    return { ok: false, reason: 'locked' }
  }
  return { ok: false, reason: 'wrong_pin', remaining: MAX_FAILED_PINS - failed }
}

/** After a successful authenticated call the server has renewed the session (spec 6.2.5);
 *  mirror that so a device used every few days does not forget a session that is still valid. */
export async function touchSession(db: Db, volunteerId: string, now: number): Promise<void> {
  const session = (await readAll(db))[volunteerId]
  if (!session) return
  const renewed = now + session.ttl_ms - DAY_MS // the server renews at most once a day
  if (renewed > session.expires_at) await saveSession(db, { ...session, expires_at: renewed })
}

export async function updateProfile(db: Db, volunteerId: string, profile: Partial<Profile>): Promise<void> {
  const session = (await readAll(db))[volunteerId]
  if (session) await saveSession(db, { ...session, profile: { ...session.profile, ...profile } })
}

/** Roles and memberships from `GET /v1/me`, plus the same renewal mirror as `touchSession`. */
export async function applyMe(db: Db, volunteerId: string, globalRoles: string[], now: number, memberships?: Membership[]): Promise<void> {
  await touchSession(db, volunteerId, now)
  const session = (await readAll(db))[volunteerId]
  if (session) await saveSession(db, { ...session, global_roles: globalRoles, ...(memberships ? { memberships } : {}) })
}

/** The volunteer's role in a shelter, from the last known memberships. */
export const roleIn = (session: LocalSession, shelterId: string): 'admin' | 'volunteer' | null =>
  session.memberships?.find((m) => m.shelter_id === shelterId)?.role ?? null
