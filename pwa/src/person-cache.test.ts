import { describe, expect, it } from 'vitest'
import type { PersonView } from './api-phase3'
import { dataToForm, formToData } from './checkin'
import { createQueue } from './queue'
import { freshDb } from './testing'
import { applyEdit, recordFromView, viewFromRecord } from './person-cache'

const view: PersonView = {
  id: 'p1', level: 'verified', biometrics_declined: false, name: 'Ana', wristband: 'B7', birth_year: 1990, is_child: false, guardians: [],
  needs: { continuous_medication: true, dietary_restriction: false, reduced_mobility: false, health_follow_up: false },
  vulnerabilities: { pregnant: false, lactating: false, disability: ['visual'] }, household_id: 'h1', current_shelter_id: 's1',
  has_thumbnail: false, history: {}, updated_at: '2026-10-21T10:00:00Z',
}

describe('the person screen without signal (spec 9.1)', () => {
  it('opens from the copy of what the server said, with the same fields', () => {
    const shown = viewFromRecord(recordFromView(view))
    expect(shown).toMatchObject({ id: 'p1', level: 'verified', name: 'Ana', wristband: 'B7', birth_year: 1990, household_id: 'h1', current_shelter_id: 's1' })
    expect(shown.needs).toEqual(view.needs)
    expect(shown.vulnerabilities).toEqual(view.vulnerabilities)
  })

  it('opens a person registered on this phone that the server has not seen', () => {
    const shown = viewFromRecord({ id: 'local', name: 'Rui', current_shelter_id: 's1', pending: true })
    expect(shown).toMatchObject({ id: 'local', level: 'no_biometrics', name: 'Rui', is_child: false, guardians: [], has_thumbnail: false })
    expect(shown.needs.continuous_medication).toBe(false)
  })

  it('the edit form works on the copy, and the edit is kept as pending with what the server will receive', async () => {
    const shown = viewFromRecord(recordFromView(view))
    const form = { ...dataToForm(shown), name: 'Ana Souza', wristband: '' }
    const { is_child: _c, child_status: _s, ...data } = formToData(form)
    const now = new Date('2026-10-21T12:00:00Z')

    const db = await freshDb()
    await db.put('people', recordFromView(view))
    const edited = applyEdit((await db.get('people', 'p1'))!, data, now)
    await db.put('people', edited)
    expect(edited).toMatchObject({ name: 'Ana Souza', wristband: 'B7', pending: true, updated_at: now.toISOString() })

    // The same data goes to the queue as the PATCH of the person: offline it waits there.
    const fetchMock = () => Promise.reject(new TypeError('offline'))
    const queue = createQueue({ db, fetch: fetchMock as typeof fetch, deviceId: async () => 'd', getStatus: async () => null })
    await queue.enqueue({ method: 'PATCH', path: '/v1/people/p1', body: { data, client_edited_at: now.toISOString() }, volunteer_id: 'ana', shelter_id: 's1' })
    const [item] = await queue.list()
    expect(item).toMatchObject({ method: 'PATCH', path: '/v1/people/p1', state: 'pending' })
    expect((item!.body as { data: { name: string } }).data.name).toBe('Ana Souza')
    // Opening again still shows the edit.
    expect(viewFromRecord((await db.get('people', 'p1'))!).name).toBe('Ana Souza')
  })

  it('the edit leaves untouched what the form did not change', () => {
    const edited = applyEdit(recordFromView(view), { needs: { ...view.needs, reduced_mobility: true } }, new Date())
    expect(edited).toMatchObject({ name: 'Ana', needs: { continuous_medication: true, reduced_mobility: true } })
  })
})
