import type {
  BiometricsRequest,
  BiometricsResponse,
  CardCancelRequest,
  CardCreateRequest,
  CardView,
  CheckinRequest,
  CheckinResponse,
  ExitRequest,
  GuardianRequest,
  HouseholdCreateRequest,
  HouseholdPatchRequest,
  HouseholdView,
  PatchPersonRequest,
  PersonSummary,
  PersonView,
} from './api-phase3'

export interface ApiStatus {
  unico: 'ok' | 'degraded'
  reason?: 'limit' | 'unavailable'
}

/** `GET /v1/status` (spec 8.1). Returns null when the server cannot be reached. */
export async function getStatus(fetchFn: typeof fetch = fetch): Promise<ApiStatus | null> {
  try {
    const res = await fetchFn('/v1/status', { cache: 'no-store' })
    return res.ok ? ((await res.json()) as ApiStatus) : null
  } catch {
    return null
  }
}

export type ApiResult<T> = { ok: true; data: T } | { ok: false; status: number; error: string; detail?: unknown }

export interface VolunteerView {
  id: string
  name: string
  phone: string | null
  show_phone: boolean
  language: 'pt-BR' | 'en'
}

export interface SessionResponse {
  session: string
  expires_at: string
  volunteer: VolunteerView
}

export interface Membership {
  shelter_id: string
  name: string
  role: 'admin' | 'volunteer'
  confirmation: 'unconfirmed' | 'confirmed'
}

export interface Me {
  volunteer: VolunteerView
  global_roles: string[]
  memberships: Membership[]
}

export interface AddressInput {
  text?: string
  city?: string
  state?: string
}

export interface ReferenceContacts {
  civil_defense_phone?: string
  child_protection_phone?: string
}

export interface EventView {
  id: string
  name: string
  auto_named: boolean
  reference_contacts: ReferenceContacts
}

/** Full view (members, validators, DC); non-members only get the first six fields (spec 8.2). */
export interface ShelterView {
  id: string
  name: string
  confirmation: 'unconfirmed' | 'confirmed'
  capacity: number | null
  occupancy: number
  vacancies: number | null
  lat?: number
  lon?: number
  accuracy_m?: number
  address?: AddressInput & { pending: boolean }
  event_id?: string | null
  event?: EventView | null
  status?: 'open' | 'closed'
  confirmation_type?: 'validator' | 'in_person' | null
  can_endorse?: boolean
  flagged?: boolean
  created_at?: string
  role?: 'admin' | 'volunteer' | null
}

export interface InviteInfo {
  valid: boolean
  shelter_name: string | null
  role: 'admin' | 'volunteer' | null
  kind: 'qr' | 'link' | null
  confirmed: boolean | null
}

export interface InviteView {
  code: string
  url: string
  kind: 'qr' | 'link'
  role: 'admin' | 'volunteer'
  created_at: string
  expires_at: string
}

export interface MemberView {
  volunteer_id: string
  name: string
  role: 'admin' | 'volunteer'
  via: 'creator' | 'qr' | 'link'
  joined_at: string
  phone?: string
}

export interface AlertView {
  id: string
  kind: string
  informational: boolean
  target_kind: string
  target_id: string
  event_id: string | null
  shelter_ids: string[]
  volunteer_ids: string[]
  detail: Record<string, unknown>
  created_at: string
  resolved: boolean
}

export interface AcceptResponse extends SessionResponse {
  membership: Membership
}

export type CapturePurpose = 'login' | 'invite' | 'checkin' | 'revalidation' | 'biometrics'

export interface GeoBody {
  lat: number
  lon: number
  accuracy: number
}

export interface ProfileInput {
  name: string
  phone?: string
  show_phone: boolean
  language: 'pt-BR' | 'en'
}

/** The calls of spec 8.1. A network failure is `{ ok: false, status: 0, error: 'network' }`. */
export function createApi(fetchFn: typeof fetch, deviceId: () => Promise<string>) {
  async function call<T>(method: string, path: string, opts: { secret?: string; body?: unknown } = {}): Promise<ApiResult<T>> {
    const headers: Record<string, string> = { 'X-Device-Id': await deviceId() }
    if (opts.body !== undefined) headers['Content-Type'] = 'application/json'
    if (opts.secret) headers.Authorization = `Bearer ${opts.secret}`
    let res: Response
    try {
      res = await fetchFn(path, { method, headers, body: opts.body === undefined ? undefined : JSON.stringify(opts.body) })
    } catch {
      return { ok: false, status: 0, error: 'network' }
    }
    const body = res.status === 204 ? null : await res.json().catch(() => null)
    // 202 is "capture_pending": a 2xx that is not a result yet.
    if (res.ok && res.status !== 202) return { ok: true, data: body as T }
    const failure = body as { error?: string; detail?: unknown } | null
    return { ok: false, status: res.status, error: failure?.error ?? `http_${res.status}`, detail: failure?.detail }
  }

  return {
    // `checkin`, `revalidation` and `biometrics` need the session and say what the capture is for.
    createCapture: (purpose: CapturePurpose = 'login', opts: { secret?: string; shelter_id?: string; expected_person_id?: string } = {}) =>
      call<{ capture_id: string; link: string; expires_at: string }>('POST', '/v1/captures', {
        secret: opts.secret,
        body: { purpose, ...(opts.shelter_id ? { shelter_id: opts.shelter_id } : {}), ...(opts.expected_person_id ? { expected_person_id: opts.expected_person_id } : {}) },
      }),
    createSession: (captureId: string, profile?: ProfileInput) =>
      call<SessionResponse>('POST', '/v1/sessions', { body: { capture_id: captureId, ...(profile ? { profile } : {}) } }),
    getMe: (secret: string) => call<Me>('GET', '/v1/me', { secret }),
    deleteSession: (secret: string) => call<null>('DELETE', '/v1/sessions/current', { secret }),

    // Shelters and events (spec 8.2, 8.3)
    getShelter: (secret: string, id: string) => call<{ shelter: ShelterView }>('GET', `/v1/shelters/${id}`, { secret }),
    patchShelter: (secret: string, id: string, body: { name?: string; address?: AddressInput; capacity?: number | null }) =>
      call<{ shelter: ShelterView }>('PATCH', `/v1/shelters/${id}`, { secret, body }),
    closeShelter: (secret: string, id: string) => call<{ shelter: ShelterView }>('POST', `/v1/shelters/${id}/close`, { secret }),
    confirmShelter: (secret: string, id: string) => call<{ shelter: ShelterView }>('POST', `/v1/shelters/${id}/confirm`, { secret }),
    revokeShelter: (secret: string, id: string, reason: string) =>
      call<{ shelter: ShelterView }>('POST', `/v1/shelters/${id}/revoke`, { secret, body: { reason } }),
    patchEvent: (secret: string, id: string, body: { name?: string; reference_contacts?: ReferenceContacts }) =>
      call<{ event: EventView }>('PATCH', `/v1/events/${id}`, { secret, body }),
    createEndorsementCode: (secret: string, id: string, body: GeoBody) =>
      call<{ code: string; url: string; expires_at: string }>('POST', `/v1/shelters/${id}/endorsements/in-person/code`, { secret, body }),
    endorse: (secret: string, body: GeoBody & { code: string; endorser_shelter_id: string }) =>
      call<{ shelter: ShelterView }>('POST', '/v1/endorsements/in-person', { secret, body }),

    // Invites and members (spec 8.4)
    createInvite: (secret: string, id: string, body: { kind: 'qr' | 'link'; role: 'admin' | 'volunteer' }) =>
      call<{ code: string; url: string; expires_at: string }>('POST', `/v1/shelters/${id}/invites`, { secret, body }),
    listInvites: (secret: string, id: string) =>
      call<{ invites: InviteView[]; joined_by_link: { volunteer_id: string; name: string; invite_code: string; joined_at: string }[] }>(
        'GET', `/v1/shelters/${id}/invites`, { secret }),
    cancelInvite: (secret: string, code: string) => call<null>('DELETE', `/v1/invites/${code}`, { secret }),
    getInvite: (code: string) => call<InviteInfo>('GET', `/v1/invites/${code}`),
    acceptInvite: (code: string, captureId: string, profile?: ProfileInput) =>
      call<AcceptResponse>('POST', `/v1/invites/${code}/accept`, { body: { capture_id: captureId, ...(profile ? { profile } : {}) } }),
    listMembers: (secret: string, id: string) => call<{ members: MemberView[] }>('GET', `/v1/shelters/${id}/members`, { secret }),
    promoteMember: (secret: string, id: string, volunteerId: string) =>
      call<{ member: MemberView }>('PATCH', `/v1/shelters/${id}/members/${volunteerId}`, { secret, body: { role: 'admin' } }),
    removeMember: (secret: string, id: string, volunteerId: string) =>
      call<null>('DELETE', `/v1/shelters/${id}/members/${volunteerId}`, { secret }),
    leaveShelter: (secret: string, id: string) => call<null>('POST', `/v1/shelters/${id}/leave`, { secret }),

    // Phase 3 (spec 8.5). The offline writes go through the queue; these are the direct calls.
    postCheckin: (secret: string, shelterId: string, body: CheckinRequest) =>
      call<CheckinResponse>('POST', `/v1/shelters/${shelterId}/checkins`, { secret, body }),
    listPeople: (secret: string, shelterId: string) => call<{ people: PersonSummary[] }>('GET', `/v1/shelters/${shelterId}/people`, { secret }),
    listNeedsSelfie: (secret: string, shelterId: string) =>
      call<{ people: PersonSummary[] }>('GET', `/v1/shelters/${shelterId}/people/needs-selfie`, { secret }),
    getPerson: (secret: string, id: string) => call<{ person: PersonView }>('GET', `/v1/people/${id}`, { secret }),
    patchPerson: (secret: string, id: string, body: PatchPersonRequest) => call<{ person: PersonView }>('PATCH', `/v1/people/${id}`, { secret, body }),
    putBiometrics: (secret: string, id: string, body: BiometricsRequest) =>
      call<BiometricsResponse>('PUT', `/v1/people/${id}/biometrics`, { secret, body }),
    postGuardian: (secret: string, id: string, body: GuardianRequest) => call<{ person: PersonView }>('POST', `/v1/people/${id}/guardians`, { secret, body }),
    /** The thumbnail is an `image/jpeg`: the caller reads the blob, so this one returns the raw response. */
    getThumbnail: (secret: string, id: string) =>
      fetchFn(`/v1/people/${id}/thumbnail`, { headers: { Authorization: `Bearer ${secret}` } }),
    postExit: (secret: string, shelterId: string, body: ExitRequest) => call<{ person: PersonView }>('POST', `/v1/shelters/${shelterId}/exits`, { secret, body }),
    postHousehold: (secret: string, shelterId: string, body: HouseholdCreateRequest) =>
      call<{ household: HouseholdView }>('POST', `/v1/shelters/${shelterId}/households`, { secret, body }),
    getHousehold: (secret: string, id: string) => call<HouseholdView>('GET', `/v1/households/${id}`, { secret }),
    patchHousehold: (secret: string, id: string, body: HouseholdPatchRequest) => call<HouseholdView>('PATCH', `/v1/households/${id}`, { secret, body }),
    postCard: (secret: string, householdId: string, body: CardCreateRequest) => call<CardView>('POST', `/v1/households/${householdId}/cards`, { secret, body }),
    getCard: (secret: string, code: string) => call<CardView>('GET', `/v1/cards/${code}`, { secret }),
    cancelCard: (secret: string, code: string, body: CardCancelRequest = {}) => call<CardView>('POST', `/v1/cards/${code}/cancel`, { secret, body }),

    // Alerts (spec 8.8)
    listAlerts: (secret: string) => call<{ alerts: AlertView[] }>('GET', '/v1/alerts', { secret }),
    resolveAlert: (secret: string, id: string) => call<Record<string, never>>('POST', `/v1/alerts/${id}/resolve`, { secret }),
  }
}

export type Api = ReturnType<typeof createApi>
