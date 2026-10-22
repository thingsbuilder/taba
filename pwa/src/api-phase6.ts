// Response types of the phase 6 routes (spec 3.10, 8.8). Types only, no logic. This file is the
// contract between the API owner of the dashboard and the screen: the API answers these shapes, and
// a change to one changes both sides. They mirror the structs of `api/src/dashboard.rs`.

export type EventStatus = 'active' | 'closed' | 'purged'

/** A shelter that waits for a confirmation: where a validator or the Civil Defense finds it without knowing an id. */
export interface ShelterToConfirm {
  id: string
  name: string
  created_at: string
  flagged: boolean
  city: string | null
  state: string | null
  lat: number
  lon: number
  created_by_name: string | null
}

/** One event of `GET /v1/dashboard/events`. */
export interface EventSummary {
  id: string
  name: string
  status: EventStatus
  created_at: string
  shelters: number
  unique_people: number
  occupancy: number
  unconfirmed_shelters: ShelterToConfirm[]
}

export interface ByLevel {
  verified: number
  assisted: number
  no_biometrics: number
}

/** The cuts of spec 3.10, for an event or for one shelter. */
export interface Counts {
  unique_people: number
  by_level: ByLevel
  /** `0_11`, `12_17`, `18_59`, `60_plus`, `unknown` (no `birth_year`). */
  age_bands: Record<string, number>
  pregnant: number
  lactating: number
  /** By kind (`mobility`, `visual`, `hearing`, `intellectual`, `psychosocial`, `other`). */
  disability: Record<string, number>
  children_separated: number
  children_unaccompanied: number
  indigenous: number
  migrant: number
  households: number
  pets: number
}

export interface ShelterRow {
  id: string
  name: string
  confirmation: 'unconfirmed' | 'confirmed'
  flagged: boolean
  lat: number
  lon: number
  capacity: number | null
  occupancy: number
  vacancies: number | null
  counts: Counts
}

/** `GET /v1/dashboard/events/{id}`. */
export interface EventDashboard {
  id: string
  name: string
  status: EventStatus
  /** The fixed notice of spec 3.10: it counts people in shelters, not those displaced elsewhere. */
  notice: string
  totals: Counts
  shelters: ShelterRow[]
  movement: { last_24h: number; last_7d: number }
  /** By `exit_destination`. */
  exits_by_destination: Record<string, number>
  rescues: number
  open_alerts: number
  open_matches: { central: number; protection: number }
  pilot: {
    /** 0 to 1; null with nobody registered. */
    selfie_refusal_rate: number | null
    not_the_person: number
    other_site_reasons: number
    unico_processes_per_person: number | null
  }
  unconfirmed_shelters: ShelterToConfirm[]
}
