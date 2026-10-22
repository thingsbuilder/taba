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
import type { EventDashboard, EventSummary } from './api-phase6'
import type { DeliveryRequest, DeliveryResponse, DeliveryStatus, DeliveryView, Items, PutItemsRequest, PutItemsResponse, ReviewRequest } from './api-phase5'
import type {
  MatchDecisionRequest,
  MatchDecisionResponse,
  MatchDetail,
  MatchSummary,
  MediatorRole,
  NearbyShelter,
  RescuePatchRequest,
  RescueRequest,
  RescueResponse,
  SearchLinkCreateRequest,
  SearchLinkCreated,
  SearchLinkStatus,
  SearchLinkSubmitRequest,
  SearchRequest,
  SearchResponse,
} from './api-phase4'
import { logEvent, routeOf } from './diagnostics'

export interface ApiStatus {
  unico: 'ok' | 'degraded'
  reason?: 'limit' | 'unavailable'
  /** `PILOT_RESPONSIBLE`, for the consent screen; null when the instance has none. */
  pilot_responsible?: string | null
}

/** `GET /v1/status` (spec 8.1). Returns null when the server cannot be reached. */
export async function getStatus(fetchFn: typeof fetch = fetch): Promise<ApiStatus | null> {
  try {
    const res = await fetchFn('/v1/status', { cache: 'no-store' })
    if (!res.ok) {
      logEvent('status_failed', { status: res.status })
      return null
    }
    const status = (await res.json()) as ApiStatus
    if (status.unico !== 'ok') logEvent('status_degraded', { reason: status.reason })
    return status
  } catch {
    logEvent('status_failed', { status: 0 })
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
  /** What the shelter gives (3.5.2); full view only. */
  items?: Items
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
  async function call<T>(method: string, path: string, opts: { secret?: string; body?: unknown; key?: string } = {}): Promise<ApiResult<T>> {
    const headers: Record<string, string> = { 'X-Device-Id': await deviceId() }
    if (opts.key) headers['Idempotency-Key'] = opts.key
    if (opts.body !== undefined) headers['Content-Type'] = 'application/json'
    if (opts.secret) headers.Authorization = `Bearer ${opts.secret}`
    let res: Response
    try {
      res = await fetchFn(path, { method, headers, body: opts.body === undefined ? undefined : JSON.stringify(opts.body) })
    } catch {
      logEvent('api_error', { method, route: routeOf(path), status: 0, error: 'network' })
      return { ok: false, status: 0, error: 'network' }
    }
    const body = res.status === 204 ? null : await res.json().catch(() => null)
    // 202 is "capture_pending": a 2xx that is not a result yet.
    if (res.ok && res.status !== 202) return { ok: true, data: body as T }
    const failure = body as { error?: string; detail?: unknown } | null
    const error = failure?.error ?? `http_${res.status}`
    logEvent('api_error', { method, route: routeOf(path), status: res.status, error })
    return { ok: false, status: res.status, error, detail: failure?.detail }
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

    // Phase 4 (spec 8.7). Rescues and searches go through the queue; these are the direct calls.
    postRescue: (secret: string, body: RescueRequest) => call<RescueResponse>('POST', '/v1/rescues', { secret, body }),
    patchRescue: (secret: string, id: string, body: RescuePatchRequest) => call<Record<string, never>>('PATCH', `/v1/rescues/${id}`, { secret, body }),
    nearbyShelters: (secret: string, lat: number, lon: number) =>
      call<{ shelters: NearbyShelter[] }>('GET', `/v1/shelters/nearby?lat=${lat}&lon=${lon}`, { secret }),
    postSearch: (secret: string, body: SearchRequest) => call<SearchResponse>('POST', '/v1/searches', { secret, body }),
    listMatches: (secret: string) => call<{ matches: MatchSummary[] }>('GET', '/v1/matches', { secret }),
    /** An `image/jpeg`: the caller reads the blob, so this one returns the raw response. */
    getMatchThumbnail: (secret: string, id: string) => fetchFn(`/v1/matches/${id}/thumbnail`, { headers: { Authorization: `Bearer ${secret}` } }),
    getMatch: (secret: string, id: string) => call<MatchDetail>('GET', `/v1/matches/${id}`, { secret }),
    decideMatch: (secret: string, id: string, body: MatchDecisionRequest) =>
      call<MatchDecisionResponse>('POST', `/v1/matches/${id}/decision`, { secret, body }),
    grantRole: (secret: string, volunteerId: string, role: MediatorRole) =>
      call<Record<string, never>>('POST', `/v1/volunteers/${volunteerId}/roles`, { secret, body: { role } }),
    revokeRole: (secret: string, volunteerId: string, role: MediatorRole) =>
      call<null>('DELETE', `/v1/volunteers/${volunteerId}/roles/${role}`, { secret }),
    // Family link: the first two need a session, the last two are public (no `secret`).
    createSearchLink: (secret: string, body: SearchLinkCreateRequest) => call<SearchLinkCreated>('POST', '/v1/search-links', { secret, body }),
    cancelSearchLink: (secret: string, code: string) => call<null>('DELETE', `/v1/search-links/${code}`, { secret }),
    getSearchLink: (code: string) => call<SearchLinkStatus>('GET', `/v1/search-links/${code}`),
    submitSearchLink: (code: string, body: SearchLinkSubmitRequest) => call<SearchResponse>('POST', `/v1/search-links/${code}/submit`, { body }),

    // Phase 5 (spec 8.6). Deliveries go through the queue; these are the direct calls.
    putItems: (secret: string, shelterId: string, body: PutItemsRequest) => call<PutItemsResponse>('PUT', `/v1/shelters/${shelterId}/items`, { secret, body }),
    /** `key` is the `Idempotency-Key`: the queue sends its item id; the direct send after a selfie sends the delivery id. */
    postDelivery: (secret: string, shelterId: string, body: DeliveryRequest, key?: string) =>
      call<DeliveryResponse>('POST', `/v1/shelters/${shelterId}/deliveries`, { secret, body, key }),
    /** `status` is required: the list is read by `(shelter, status)`. */
    listDeliveries: (secret: string, shelterId: string, status: DeliveryStatus) =>
      call<{ deliveries: DeliveryView[] }>('GET', `/v1/shelters/${shelterId}/deliveries?status=${status}`, { secret }),
    reviewDelivery: (secret: string, id: string, body: ReviewRequest) => call<Record<string, never>>('POST', `/v1/deliveries/${id}/review`, { secret, body }),

    // Phase 6 (spec 3.10, 8.8). The two CSVs are files: the caller reads the blob, so they return the raw response.
    getDashboardEvents: (secret: string) => call<{ events: EventSummary[] }>('GET', '/v1/dashboard/events', { secret }),
    getDashboardEvent: (secret: string, id: string) => call<EventDashboard>('GET', `/v1/dashboard/events/${id}`, { secret }),
    getDailyCsv: (secret: string, id: string) => fetchFn(`/v1/dashboard/events/${id}/daily.csv`, { headers: { Authorization: `Bearer ${secret}` } }),
    getShelterExport: (secret: string, shelterId: string) => fetchFn(`/v1/shelters/${shelterId}/export.csv`, { headers: { Authorization: `Bearer ${secret}` } }),

    // Alerts (spec 8.8)
    listAlerts: (secret: string) => call<{ alerts: AlertView[] }>('GET', '/v1/alerts', { secret }),
    resolveAlert: (secret: string, id: string) => call<Record<string, never>>('POST', `/v1/alerts/${id}/resolve`, { secret }),
  }
}

export type Api = ReturnType<typeof createApi>
