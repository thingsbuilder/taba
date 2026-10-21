// Request and response types of the phase 3 routes (spec 7, 8.5, doc da fase 3). Types only, no
// logic. The server shapes are the spec's; where the spec leaves a response open the shape below is
// provisional and the owner of the route (see docs/pt_br/fases/fase-3-contrato.md) adjusts it
// together with the API.

export type Level = 'verified' | 'assisted' | 'no_biometrics'
export type LegalBasis = 'consent' | 'vital_interest' | 'child_protection'
export type ChildStatus = 'with_guardian' | 'separated' | 'unaccompanied'
export type GuardianLink = 'declared' | 'document_checked'
export type ExitDestination = 'own_home' | 'relatives_or_friends' | 'rental_aid' | 'other_shelter' | 'other' | 'unknown'
export type ExitReason = 'checkin_elsewhere' | 'exit' | 'exceptional_exit'
export type Disability = 'mobility' | 'visual' | 'hearing' | 'intellectual' | 'psychosocial' | 'other'

/** The four closed yes/no needs (spec 3.4); nothing else is accepted. */
export interface Needs {
  continuous_medication: boolean
  dietary_restriction: boolean
  reduced_mobility: boolean
  health_follow_up: boolean
}

export interface Vulnerabilities {
  pregnant: boolean
  lactating: boolean
  disability: Disability[]
  indigenous_people?: string
  migrant_language?: string
}

/** `data` of a check-in or of `PATCH /v1/people/{id}`; everything is optional except `is_child`. */
export interface PersonData {
  name?: string
  social_name?: string
  wristband?: string
  birth_year?: number
  is_child: boolean
  child_status?: ChildStatus
  needs?: Needs
  vulnerabilities?: Vulnerabilities
}

// ---- check-in (8.5) ----

export interface CheckinRequest {
  id: string
  /** Exactly one of the three ways (spec 3.4 paths A, B, C). */
  capture_id?: string
  photo_base64?: string
  no_biometrics?: true
  biometrics_declined?: boolean
  data: PersonData
  thumbnail_base64?: string
  legal_basis: LegalBasis
  client_created_at: string
}

export interface StayView {
  shelter_id: string
  shelter_name?: string
  entered_at: string
  left_at?: string | null
  exit_reason?: ExitReason | null
}

/** By 4.11: a volunteer of a confirmed shelter gets only `has_other_stays`; an admin gets `stays`. */
export interface History {
  has_other_stays?: boolean
  stays?: StayView[]
}

export interface CheckinResponse {
  person_id: string
  level: Level
  is_new: boolean
  history: History
}

export interface Guardian {
  person_id: string
  link: GuardianLink
  checked_by?: string
  checked_at?: string
}

export interface PersonView {
  id: string
  level: Level
  biometrics_declined: boolean
  name?: string
  social_name?: string
  wristband?: string
  birth_year?: number
  is_child: boolean
  child_status?: ChildStatus
  guardians: Guardian[]
  needs: Needs
  vulnerabilities: Vulnerabilities
  household_id?: string
  current_shelter_id?: string
  has_thumbnail: boolean
  history: History
  updated_at: string
}

/** One row of `GET /v1/shelters/{id}/people` and of the needs-selfie list. */
export interface PersonSummary {
  id: string
  level: Level
  name?: string
  wristband?: string
  is_child: boolean
  child_status?: ChildStatus
  household_id?: string
  has_thumbnail: boolean
  updated_at: string
}

export interface PatchPersonRequest {
  data: Partial<PersonData>
  /** Last writer wins by this time (spec 9.4). */
  client_edited_at: string
}

export interface BiometricsRequest {
  capture_id?: string
  photo_base64?: string
}

export interface BiometricsResponse {
  person_id: string
  level: Level
}

export interface GuardianRequest {
  guardian_id: string
  link: GuardianLink
}

// ---- exit (8.5) ----

export interface ExitRequest {
  person_id: string
  exit_destination: ExitDestination
  destination_municipality?: string
  guardian_id?: string
  capture_id?: string
  /** Only `protection` or the Civil Defense (spec 3.8). */
  exception?: { reason: string }
}

// ---- households and cards (8.5) ----

export interface Pet {
  species: string
  count: number
}

export interface HouseholdCreateRequest {
  id: string
  member_ids: string[]
  head_id?: string
  pets?: Pet[]
  client_created_at: string
}

export interface HouseholdPatchRequest {
  add_member_ids?: string[]
  remove_member_ids?: string[]
  head_id?: string
  pets?: Pet[]
}

/** Never carries the current shelter nor the history (spec 10.3). */
export interface MemberRef {
  person_id: string
  name?: string
  level: Level
}

export interface HouseholdView {
  id: string
  members: MemberRef[]
  head_id?: string
  pets: Pet[]
  card_code?: string
}

export interface CardCreateRequest {
  code: string
  client_issued_at: string
}

export interface CardView {
  code: string
  status: 'active' | 'canceled'
  household_id: string
  members: MemberRef[]
}

export interface CardCancelRequest {
  replaced_by?: string
}
