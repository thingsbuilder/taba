import type { CheckinRequest, ChildStatus, Disability, LegalBasis, Needs, PersonData, Vulnerabilities } from './api-phase3'
import { errorKey } from './app-context'
import { enqueueDegradedCheckin } from './degraded'
import type { Queue } from './queue'

// Check-in (spec 3.4): the form, its checks and the requests of the three ways to register.

export const NEED_KEYS: (keyof Needs)[] = ['continuous_medication', 'dietary_restriction', 'reduced_mobility', 'health_follow_up']
export const DISABILITIES: Disability[] = ['mobility', 'visual', 'hearing', 'intellectual', 'psychosocial', 'other']
export const CHILD_STATUSES: ChildStatus[] = ['with_guardian', 'separated', 'unaccompanied']
export const MIN_BIRTH_YEAR = 1900

export interface PersonForm {
  name: string
  social_name: string
  wristband: string
  birth_year: string
  is_child: boolean
  child_status: ChildStatus | ''
  needs: Needs
  pregnant: boolean
  lactating: boolean
  disability: Disability[]
  indigenous_people: string
  migrant_language: string
}

export const emptyNeeds = (): Needs => ({ continuous_medication: false, dietary_restriction: false, reduced_mobility: false, health_follow_up: false })

export const emptyForm = (): PersonForm => ({
  name: '',
  social_name: '',
  wristband: '',
  birth_year: '',
  is_child: false,
  child_status: '',
  needs: emptyNeeds(),
  pregnant: false,
  lactating: false,
  disability: [],
  indigenous_people: '',
  migrant_language: '',
})

/** The `data` of the request: trimmed, empty text left out, `needs` always the four booleans (spec 3.4). */
export function formToData(form: PersonForm): PersonData {
  const text = (v: string) => v.trim() || undefined
  const vulnerabilities: Vulnerabilities = {
    pregnant: form.pregnant,
    lactating: form.lactating,
    disability: form.disability,
    ...(text(form.indigenous_people) ? { indigenous_people: text(form.indigenous_people) } : {}),
    ...(text(form.migrant_language) ? { migrant_language: text(form.migrant_language) } : {}),
  }
  return {
    ...(text(form.name) ? { name: text(form.name) } : {}),
    ...(text(form.social_name) ? { social_name: text(form.social_name) } : {}),
    ...(text(form.wristband) ? { wristband: text(form.wristband) } : {}),
    ...(text(form.birth_year) ? { birth_year: Number(form.birth_year) } : {}),
    is_child: form.is_child,
    ...(form.is_child && form.child_status ? { child_status: form.child_status } : {}),
    needs: { ...form.needs },
    vulnerabilities,
  }
}

/** The form of an existing person, to edit it. */
export function dataToForm(p: {
  name?: string; social_name?: string; wristband?: string; birth_year?: number; is_child: boolean
  child_status?: ChildStatus; needs: Needs; vulnerabilities: Vulnerabilities
}): PersonForm {
  return {
    name: p.name ?? '',
    social_name: p.social_name ?? '',
    wristband: p.wristband ?? '',
    birth_year: p.birth_year === undefined ? '' : String(p.birth_year),
    is_child: p.is_child,
    child_status: p.child_status ?? '',
    needs: { ...p.needs },
    pregnant: p.vulnerabilities.pregnant,
    lactating: p.vulnerabilities.lactating,
    disability: [...p.vulnerabilities.disability],
    indigenous_people: p.vulnerabilities.indigenous_people ?? '',
    migrant_language: p.vulnerabilities.migrant_language ?? '',
  }
}

export type FormError = 'name_or_wristband' | 'birth_year' | 'child_status'

/** Everything in the form is optional, except what a way of registering cannot do without. */
export function validateForm(form: PersonForm, opts: { needsIdentity: boolean; now?: Date }): FormError[] {
  const errors: FormError[] = []
  const year = form.birth_year.trim()
  if (year !== '') {
    const n = Number(year)
    if (!Number.isInteger(n) || n < MIN_BIRTH_YEAR || n > (opts.now ?? new Date()).getFullYear()) errors.push('birth_year')
  }
  if (form.is_child && form.child_status === '') errors.push('child_status')
  // Without a face there is only the name or the wristband to find the person again (spec 3.4, path C).
  if (opts.needsIdentity && form.name.trim() === '' && form.wristband.trim() === '') errors.push('name_or_wristband')
  return errors
}

/** A person without name and without wristband gets the 128x128 thumbnail (spec 4.6). */
export const needsThumbnail = (data: Pick<PersonData, 'name' | 'wristband'>): boolean => !data.name && !data.wristband

interface BodyInput {
  id: string
  data: PersonData
  legal_basis: LegalBasis
  now: Date
}

function base(i: BodyInput) {
  return {
    id: i.id,
    data: i.data,
    legal_basis: i.legal_basis,
    client_created_at: i.now.toISOString(),
  }
}

/** Path A: the selfie with liveness already happened at Unico (spec 3.4). */
export const bodyWithCapture = (i: BodyInput & { capture_id: string }): CheckinRequest => ({ ...base(i), capture_id: i.capture_id })

/** Path C: no biometrics; `declined` records that the person refused the selfie. `person_id` is `id`. */
export const bodyNoBiometrics = (i: BodyInput & { declined: boolean }): CheckinRequest => ({
  ...base(i),
  no_biometrics: true,
  ...(i.declined ? { biometrics_declined: true } : {}),
})

export interface QueuedInput extends BodyInput {
  shelterId: string
  volunteerId: string | null
  /** The reduced JPEG taken with the app camera; absent for the no-photo way. */
  photo?: Blob
  /** The person refused the selfie (spec 3.4, path C). */
  declined: boolean
  /** `GET /status` says the facial recognition is down (spec 5.7). */
  degraded: boolean
}

/**
 * What to put in the queue (spec 3.4, 5.7). With a photo and Unico working, the photo is the check-in (path B,
 * `assisted`). With Unico down the check-in goes by path C and the photo waits as a late biometrics `PUT` that
 * the queue sends after it (`enqueueDegradedCheckin`, strict FIFO). `thumbnail` is only kept for a person without
 * name and wristband. Photos go to the queue as `Blob`s: it encrypts them and puts them back when sending (6.1.7).
 */
export async function enqueueCheckin(queue: Pick<Queue, 'enqueue'>, i: QueuedInput & { thumbnail?: Blob }): Promise<void> {
  const thumbnail = i.thumbnail && needsThumbnail(i.data) ? i.thumbnail : undefined
  const who = { volunteer_id: i.volunteerId, shelter_id: i.shelterId }
  if (i.photo && i.degraded) {
    await enqueueDegradedCheckin(queue, base(i), i.photo, who, thumbnail)
    return
  }
  const body = i.photo ? base(i) : bodyNoBiometrics(i)
  const blobs = { ...(i.photo ? { photo: i.photo } : {}), ...(thumbnail ? { thumbnail } : {}) }
  await queue.enqueue(
    { method: 'POST', path: `/v1/shelters/${i.shelterId}/checkins`, body, ...who },
    Object.keys(blobs).length > 0 ? blobs : undefined,
  )
}

const OWN_ERRORS = new Set(['no_face', 'invalid_image', 'image_too_large'])

/** i18n key for a server error of the check-in screens; the rest are the app's common ones. */
export const checkinErrorKey = (code: string): string => (OWN_ERRORS.has(code) ? `checkin.error.${code}` : errorKey(code))
