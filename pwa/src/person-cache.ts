import type { Needs, PersonData, PersonView, Vulnerabilities } from './api-phase3'
import type { PersonRecord } from './db'

// The person screen from the phone's own copy (spec 9.1: editing a person works offline).

const NO_NEEDS: Needs = { continuous_medication: false, dietary_restriction: false, reduced_mobility: false, health_follow_up: false }
const NO_VULNERABILITIES: Vulnerabilities = { pregnant: false, lactating: false, disability: [] }

/** What the server said, kept for the next time without signal. Guardians, history and the thumbnail need the server. */
export function recordFromView(person: PersonView): PersonRecord {
  return {
    id: person.id,
    level: person.level,
    name: person.name,
    social_name: person.social_name,
    wristband: person.wristband,
    birth_year: person.birth_year,
    is_child: person.is_child,
    child_status: person.child_status,
    household_id: person.household_id,
    current_shelter_id: person.current_shelter_id,
    needs: person.needs,
    vulnerabilities: person.vulnerabilities,
    biometrics_declined: person.biometrics_declined,
    pending: false,
    updated_at: person.updated_at,
  }
}

export function viewFromRecord(record: PersonRecord): PersonView {
  return {
    id: record.id,
    level: record.level ?? 'no_biometrics',
    biometrics_declined: record.biometrics_declined ?? false,
    name: record.name,
    social_name: record.social_name,
    wristband: record.wristband,
    birth_year: record.birth_year,
    is_child: record.is_child ?? false,
    child_status: record.child_status,
    guardians: [],
    needs: record.needs ?? NO_NEEDS,
    vulnerabilities: record.vulnerabilities ?? NO_VULNERABILITIES,
    household_id: record.household_id,
    current_shelter_id: record.current_shelter_id,
    has_thumbnail: false,
    history: {},
    updated_at: record.updated_at ?? '',
  }
}

/** The edit the volunteer made, applied to the copy at once; it stays `pending` until the queue sends it. */
export function applyEdit(record: PersonRecord, data: Partial<PersonData>, now: Date): PersonRecord {
  return {
    ...record,
    ...(data.name !== undefined ? { name: data.name } : {}),
    ...(data.social_name !== undefined ? { social_name: data.social_name } : {}),
    ...(data.wristband !== undefined ? { wristband: data.wristband } : {}),
    ...(data.birth_year !== undefined ? { birth_year: data.birth_year } : {}),
    ...(data.needs ? { needs: data.needs } : {}),
    ...(data.vulnerabilities ? { vulnerabilities: data.vulnerabilities } : {}),
    pending: true,
    updated_at: now.toISOString(),
  }
}
