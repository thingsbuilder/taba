import { describe, expect, it } from 'vitest'
import { buildExit, canRegisterChildExit, childExitRoute, exitErrorKey, isProtectionOrDc, queuedExit } from './exit'

describe('childExitRoute (matrix 2.3)', () => {
  it('any volunteer only for with_guardian and a document-checked guardian', () => {
    expect(childExitRoute({ child_status: 'with_guardian' }, { link: 'document_checked' })).toBe('with_guardian')
  })
  it.each([
    ['declared guardian', { child_status: 'with_guardian' as const }, { link: 'declared' as const }],
    ['separated', { child_status: 'separated' as const }, { link: 'document_checked' as const }],
    ['unaccompanied', { child_status: 'unaccompanied' as const }, { link: 'document_checked' as const }],
    ['no guardian', { child_status: 'with_guardian' as const }, undefined],
  ])('%s is protection only', (_n, child, guardian) => {
    expect(childExitRoute(child, guardian)).toBe('protection_only')
  })
})

describe('who registers', () => {
  it('admin or volunteer: only the with_guardian route', () => {
    expect(canRegisterChildExit('with_guardian', [])).toBe(true)
    expect(canRegisterChildExit('protection_only', [])).toBe(false)
    expect(canRegisterChildExit('protection_only', ['validator', 'mediator'])).toBe(false)
  })
  it('protection and the Civil Defense register both', () => {
    expect(canRegisterChildExit('protection_only', ['protection'])).toBe(true)
    expect(canRegisterChildExit('protection_only', ['civil_defense'])).toBe(true)
    expect(isProtectionOrDc(['mediator'])).toBe(false)
  })
})

describe('buildExit', () => {
  it('requires the destination (criterion 37)', () => {
    expect(buildExit({ personId: 'p', destination: '', municipality: '' })).toBeNull()
  })
  it('adult: person and destination, municipality only when written', () => {
    expect(buildExit({ personId: 'p', destination: 'own_home', municipality: ' ' })).toEqual({ person_id: 'p', exit_destination: 'own_home' })
    expect(buildExit({ personId: 'p', destination: 'other_shelter', municipality: ' Canoas ' })).toEqual({
      person_id: 'p', exit_destination: 'other_shelter', destination_municipality: 'Canoas',
    })
  })
  it('child: guardian and capture; exception only with a reason', () => {
    expect(buildExit({ personId: 'c', destination: 'relatives_or_friends', municipality: '', guardianId: 'g', captureId: 'cap' })).toEqual({
      person_id: 'c', exit_destination: 'relatives_or_friends', guardian_id: 'g', capture_id: 'cap',
    })
    expect(buildExit({ personId: 'c', destination: 'unknown', municipality: '', exceptionReason: ' ' })).not.toHaveProperty('exception')
    expect(buildExit({ personId: 'c', destination: 'unknown', municipality: '', exceptionReason: 'Juiz' })).toMatchObject({ exception: { reason: 'Juiz' } })
  })
})

describe('queuedExit', () => {
  it('an adult exit goes through the queue of the shelter', () => {
    expect(queuedExit('s1', 'v1', { person_id: 'p', exit_destination: 'unknown' })).toEqual({
      method: 'POST', path: '/v1/shelters/s1/exits', body: { person_id: 'p', exit_destination: 'unknown' }, volunteer_id: 'v1', shelter_id: 's1',
    })
  })
})

describe('exitErrorKey', () => {
  it('explains the child rules in its own words', () => {
    expect(exitErrorKey('protection_role_required')).toBe('child.error.protection_role_required')
    expect(exitErrorKey('revalidation_failed')).toBe('child.error.revalidation_failed')
    expect(exitErrorKey('forbidden')).toBe('error.forbidden')
  })
})
