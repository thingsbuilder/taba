// Request and response types of the phase 4 routes (spec 7, 8.2, 8.4, 8.7, doc da fase 4). Types
// only, no logic. This file is the contract between the API owners and the screens: the API
// answers these shapes, and a change to one changes both sides.

import type { Level } from './api-phase3'

export type FamilyLanguage = 'pt-BR' | 'en' | 'es'
export type MediatorKind = 'shelter' | 'central' | 'protection'
export type MatchStatus = 'open' | 'accepted' | 'declined' | 'not_the_person'
export type MatchDecision = 'accept' | 'decline' | 'not_the_person'
export type DecidedBy = 'person' | 'operator'
export type MediatorRole = 'mediator' | 'protection'

// ---- rescue (3.6, 8.7) ----

export interface RescueRequest {
  id: string
  photo_base64?: string
  lat: number
  lon: number
  accuracy: number
  captured_at: string
  data?: { name?: string; wristband?: string; note?: string }
  landing_point?: string
  destination_shelter_id?: string
  /** Only when there is no name and no wristband (4.6). */
  thumbnail_base64?: string
}

export interface RescueResponse {
  rescue_id: string
  person_id: string
  level: Level
}

export interface RescuePatchRequest {
  landing_point?: string
  destination_shelter_id?: string
}

/** One open shelter of `GET /v1/shelters/nearby?lat&lon` (8.2), nearest first. */
export interface NearbyShelter {
  id: string
  name: string
  distance_m: number
  capacity: number | null
  occupancy: number
  vacancies: number | null
  confirmation: 'unconfirmed' | 'confirmed'
}

// ---- search (3.7, 8.7) ----

export interface MissingPerson {
  name: string
  nickname?: string
  notes: string
}

export interface Family {
  name: string
  phone: string
  relationship: string
  language: FamilyLanguage
}

export interface SearchRequest {
  id: string
  /** 1 to 5 photos, each at most 800 KB. */
  photos_base64: string[]
  missing_person: MissingPerson
  family: Family
}

/** The same answer with or without a match (3.7 item 6); also what the family link answers. */
export interface SearchResponse {
  message: 'search_registered'
  usable_photos: number
}

// ---- family link (3.7 item 11, 8.7) ----

export interface SearchLinkCreateRequest {
  shelter_id: string
}

export interface SearchLinkCreated {
  code: string
  url: string
  expires_at: string
}

/** `GET /v1/search-links/{code}`, public: nothing about who made it nor the shelter. */
export interface SearchLinkStatus {
  valid: boolean
}

/** `POST /v1/search-links/{code}/submit`, public; answers `SearchResponse`. */
export interface SearchLinkSubmitRequest {
  photos_base64: string[]
  missing_person: MissingPerson
  family: Omit<Family, 'language'> & { language: FamilyLanguage }
}

// ---- matches (4.8, 8.7) ----

/** An item of `GET /v1/matches`: the queues the volunteer attends. No contact of the family. */
export interface MatchSummary {
  id: string
  status: MatchStatus
  mediator_kind: MediatorKind
  is_child: boolean
  missing_person_name: string
  shelter_name?: string
  created_at: string
}

/** `GET /v1/matches/{id}`: the check list. The family's phone is not here, only `accept` gives it. */
export interface MatchDetail {
  id: string
  status: MatchStatus
  mediator_kind: MediatorKind
  is_child: boolean
  /** What the family told. */
  missing_person: MissingPerson
  family: { name: string; relationship: string; language: FamilyLanguage }
  /** What the registry has of the person found. */
  person: { name?: string; social_name?: string; wristband?: string; has_thumbnail: boolean }
  shelter_name?: string
  created_at: string
  /** Only for `mediator`, `protection` and the Civil Defense, and only while the match is open (4.11). */
  history?: { stays: { shelter_id: string; entered_at: string; left_at?: string | null; exit_reason?: string | null }[] }
}

export interface MatchDecisionRequest {
  decision: MatchDecision
  decided_by: DecidedBy
  note?: string
}

/** `family` and `whatsapp_link` come only with `accept`. */
export interface MatchDecisionResponse {
  family?: Family
  whatsapp_link?: string
}

// ---- roles (2.1, 8.4) ----

export interface RoleRequest {
  role: MediatorRole
}
