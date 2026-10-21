import { describe, expect, it } from 'vitest'
import {
  bodyNoBiometrics, bodyWithCapture, checkinErrorKey, dataToForm, emptyForm, formToData, needsThumbnail, queuedCheckin, validateForm,
} from './checkin'

const now = new Date('2026-10-21T12:00:00Z')

describe('formToData', () => {
  it('leaves empty text out and always sends the four needs (spec 3.4)', () => {
    const data = formToData({ ...emptyForm(), name: '  Ana  ', wristband: '0217' })
    expect(data).toEqual({
      name: 'Ana',
      wristband: '0217',
      is_child: false,
      needs: { continuous_medication: false, dietary_restriction: false, reduced_mobility: false, health_follow_up: false },
      vulnerabilities: { pregnant: false, lactating: false, disability: [] },
    })
  })

  it('carries the year, the child status only for a child and the vulnerabilities', () => {
    const form = { ...emptyForm(), birth_year: '2019', is_child: true, child_status: 'unaccompanied' as const, pregnant: true, disability: ['visual' as const], migrant_language: 'Kreyòl' }
    expect(formToData(form)).toMatchObject({
      birth_year: 2019,
      is_child: true,
      child_status: 'unaccompanied',
      vulnerabilities: { pregnant: true, disability: ['visual'], migrant_language: 'Kreyòl' },
    })
    expect(formToData({ ...form, is_child: false })).not.toHaveProperty('child_status')
  })

  it('round-trips through dataToForm', () => {
    const form = { ...emptyForm(), name: 'Ana', birth_year: '1980', needs: { ...emptyForm().needs, reduced_mobility: true }, lactating: true, indigenous_people: 'Guarani' }
    const data = formToData(form)
    expect(dataToForm({ ...data, needs: data.needs!, vulnerabilities: data.vulnerabilities! })).toEqual(form)
  })
})

describe('validateForm', () => {
  it('accepts an empty form for the paths with a photo or selfie', () => {
    expect(validateForm(emptyForm(), { needsIdentity: false, now })).toEqual([])
  })
  it('asks for name or wristband only without biometrics (path C)', () => {
    expect(validateForm(emptyForm(), { needsIdentity: true, now })).toEqual(['name_or_wristband'])
    expect(validateForm({ ...emptyForm(), wristband: '12' }, { needsIdentity: true, now })).toEqual([])
    expect(validateForm({ ...emptyForm(), name: ' ' }, { needsIdentity: true, now })).toEqual(['name_or_wristband'])
  })
  it('checks the birth year', () => {
    const check = (y: string) => validateForm({ ...emptyForm(), birth_year: y }, { needsIdentity: false, now })
    expect(check('1899')).toEqual(['birth_year'])
    expect(check('2027')).toEqual(['birth_year'])
    expect(check('19x0')).toEqual(['birth_year'])
    expect(check('1980.5')).toEqual(['birth_year'])
    expect(check('2026')).toEqual([])
  })
  it('requires the child status of a child', () => {
    expect(validateForm({ ...emptyForm(), is_child: true }, { needsIdentity: false, now })).toEqual(['child_status'])
    expect(validateForm({ ...emptyForm(), is_child: true, child_status: 'with_guardian' }, { needsIdentity: false, now })).toEqual([])
  })
})

describe('thumbnail rule (spec 4.6)', () => {
  it('only for a person with neither name nor wristband', () => {
    expect(needsThumbnail({})).toBe(true)
    expect(needsThumbnail({ name: 'Ana' })).toBe(false)
    expect(needsThumbnail({ wristband: '7' })).toBe(false)
  })
  it('the body carries the thumbnail only then', () => {
    const base = { id: 'p1', legal_basis: 'consent' as const, now, thumbnail_base64: 'dGg=' }
    expect(bodyNoBiometrics({ ...base, data: { is_child: true }, declined: false })).toHaveProperty('thumbnail_base64', 'dGg=')
    expect(bodyNoBiometrics({ ...base, data: { is_child: true, name: 'Bia' }, declined: false })).not.toHaveProperty('thumbnail_base64')
  })
})

describe('request bodies', () => {
  const base = { id: 'p1', data: { is_child: false, name: 'Ana' }, legal_basis: 'consent' as const, now }
  it('path A names the capture', () => {
    expect(bodyWithCapture({ ...base, capture_id: 'c1' })).toEqual({
      id: 'p1', data: base.data, legal_basis: 'consent', client_created_at: now.toISOString(), capture_id: 'c1',
    })
  })
  it('path C says no_biometrics and records the refusal only when there was one', () => {
    expect(bodyNoBiometrics({ ...base, declined: true })).toMatchObject({ no_biometrics: true, biometrics_declined: true })
    expect(bodyNoBiometrics({ ...base, declined: false })).not.toHaveProperty('biometrics_declined')
  })
})

describe('queuedCheckin', () => {
  const base = { id: 'p1', data: { is_child: false, name: 'Ana' }, legal_basis: 'consent' as const, now, shelterId: 's1', volunteerId: 'v1', declined: false, degraded: false }

  it('no photo: one check-in without biometrics', () => {
    const items = queuedCheckin({ ...base, declined: true })
    expect(items).toHaveLength(1)
    expect(items[0]).toMatchObject({ method: 'POST', path: '/v1/shelters/s1/checkins', shelter_id: 's1', volunteer_id: 'v1' })
    expect(items[0]!.body).toMatchObject({ no_biometrics: true, biometrics_declined: true })
  })

  it('photo: path B, assisted by the server, no no_biometrics flag', () => {
    const [item, ...rest] = queuedCheckin({ ...base, photo_base64: 'Zm90bw==' })
    expect(rest).toEqual([])
    expect(item!.body).toMatchObject({ photo_base64: 'Zm90bw==' })
    expect(item!.body).not.toHaveProperty('no_biometrics')
  })

  it('photo with Unico down: path C first, then the photo as late biometrics (FIFO order)', () => {
    const items = queuedCheckin({ ...base, photo_base64: 'Zm90bw==', degraded: true })
    expect(items.map((i) => `${i.method} ${i.path}`)).toEqual(['POST /v1/shelters/s1/checkins', 'PUT /v1/people/p1/biometrics'])
    expect(items[0]!.body).toMatchObject({ no_biometrics: true })
    expect(items[0]!.body).not.toHaveProperty('photo_base64')
    expect(items[0]!.body).not.toHaveProperty('biometrics_declined')
    expect(items[1]!.body).toEqual({ photo_base64: 'Zm90bw==' })
  })
})

describe('checkinErrorKey', () => {
  it('has its own text for the face errors and falls back to the common ones', () => {
    expect(checkinErrorKey('no_face')).toBe('checkin.error.no_face')
    expect(checkinErrorKey('image_too_large')).toBe('checkin.error.image_too_large')
    expect(checkinErrorKey('unico_degraded')).toBe('error.unico_degraded')
    expect(checkinErrorKey('whatever')).toBe('error.generic')
  })
})
