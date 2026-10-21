import { expect, it } from 'vitest'
import { errorKey } from './app-context'
import { checkinErrorKey } from './checkin'
import { exitErrorKey } from './exit'
import en from './i18n/en.json'
import ptBR from './i18n/pt-BR.json'
import { queueErrorKey } from './queue-errors'

// Every code the phase 3 API can answer (grep of the `ApiError::new` and `invalid(...)` of people, exits,
// children, households, cards, thumbnail and the captures) and the codes the screens raise themselves.
const SERVER = [
  'guardian_not_verified', 'protection_role_required', 'revalidation_failed', 'invalid_request', 'person_not_found', 'household_not_found',
  'not_in_shelter', 'person_mismatch', 'capture_target_required', 'capture_wrong_target', 'wrong_capture_target', 'invalid_person',
  'invalid_household', 'invalid_card', 'invalid_guardian', 'household_exists', 'card_not_found', 'thumbnail_not_found', 'invalid_thumbnail',
  'person_changed', 'person_in_other_household', 'household_has_card', 'code_taken', 'exit_destination_required', 'guardian_required',
  'guardian_not_linked', 'capture_required', 'exception_for_child_only', 'exception_reason_required', 'invalid_destination_municipality',
  'event_closed', 'photo_rejected', 'no_face', 'invalid_image', 'image_too_large', 'unico_degraded', 'unico_limit', 'forbidden', 'shelter_not_found',
  'capture_consumed', 'capture_expired', 'capture_not_found', 'capture_wrong_device',
]
const LOCAL = ['photo_too_big', 'selfie_timeout', 'destination_required', 'invalid_reason']

it.each([
  ['errorKey', errorKey],
  ['checkinErrorKey', checkinErrorKey],
  ['exitErrorKey', exitErrorKey],
  ['queueErrorKey', queueErrorKey],
])('%s gives every phase 3 code its own text in Portuguese and English', (_name, keyOf) => {
  for (const code of [...SERVER, ...LOCAL]) {
    // The codes a screen never meets (the check-in screen has no exit codes) may still be generic there.
    const key = keyOf(code)
    if (key === 'error.generic') continue
    expect(ptBR, `${code} -> ${key}`).toHaveProperty([key])
    expect(en, `${code} -> ${key}`).toHaveProperty([key])
  }
})

it('no screen answers an exit or person code with the generic text', () => {
  for (const code of [...SERVER, ...LOCAL]) {
    expect(exitErrorKey(code), `exit ${code}`).not.toBe('error.generic')
    // `destination_required` is only the exit screen's.
    if (code !== 'destination_required') expect(checkinErrorKey(code), `checkin ${code}`).not.toBe('error.generic')
  }
})

it('the screens\' own errors have their own texts', () => {
  expect(checkinErrorKey('photo_too_big')).toBe('checkin.photo_too_big')
  expect(checkinErrorKey('selfie_timeout')).toBe('checkin.selfie_timeout')
  expect(exitErrorKey('destination_required')).toBe('exit.destination_required')
  expect(exitErrorKey('selfie_timeout')).toBe('checkin.selfie_timeout')
  expect(exitErrorKey('invalid_reason')).toBe('error.invalid_reason')
})

it('gives every phase 4 code its own text in Portuguese and English', () => {
  for (const code of ['search_limit', 'match_already_decided', 'event_closed', 'search_link_invalid']) {
    const key = errorKey(code)
    expect(key, code).not.toBe('error.generic')
    expect(ptBR, key).toHaveProperty([key])
    expect(en, key).toHaveProperty([key])
  }
})
