import { describe, expect, it } from 'vitest'
import type { PersonSummary } from './api-phase3'
import type { QueueItem } from './db'
import { cachePeople, cachedPeople, displayName, initials, matchesQuery, pendingPeople, relativeTime } from './people'
import { freshDb } from './testing'

describe('displayName', () => {
  it('prefers the social name, then the name, then the wristband', () => {
    expect(displayName({ name: 'Maria Souza', social_name: 'Mario' })).toEqual({ kind: 'name', text: 'Mario' })
    expect(displayName({ name: 'Maria Souza', wristband: '12' })).toEqual({ kind: 'name', text: 'Maria Souza' })
    expect(displayName({ wristband: '0217' })).toEqual({ kind: 'wristband', text: '0217' })
  })
  it('is none (never "unknown") for a person with neither', () => {
    expect(displayName({ name: ' ' })).toEqual({ kind: 'none' })
  })
})

describe('initials', () => {
  it('first and last word; # for a wristband or nobody', () => {
    expect(initials({ kind: 'name', text: 'maria da Silva' })).toBe('MS')
    expect(initials({ kind: 'name', text: 'Ana' })).toBe('A')
    expect(initials({ kind: 'wristband', text: '1' })).toBe('#')
    expect(initials({ kind: 'none' })).toBe('#')
  })
})

describe('matchesQuery', () => {
  it('ignores case and accents and looks at name, social name and wristband', () => {
    expect(matchesQuery({ name: 'José Álvares' }, 'jose alv')).toBe(true)
    expect(matchesQuery({ social_name: 'Bia' }, 'BIA')).toBe(true)
    expect(matchesQuery({ wristband: '0217' }, '217')).toBe(true)
    expect(matchesQuery({ name: 'Ana' }, 'Bia')).toBe(false)
    expect(matchesQuery({}, '')).toBe(true)
  })
})

describe('relativeTime', () => {
  const now = Date.parse('2026-10-21T12:00:00Z')
  it('says minutes ago in Portuguese and English', () => {
    expect(relativeTime('2026-10-21T11:58:00Z', now, 'pt-BR')).toBe('há 2 minutos')
    expect(relativeTime('2026-10-21T11:58:00Z', now, 'en')).toBe('2 minutes ago')
  })
  it('uses hours and days further back and says now for the last seconds', () => {
    expect(relativeTime('2026-10-21T09:00:00Z', now, 'en')).toBe('3 hours ago')
    expect(relativeTime('2026-10-19T12:00:00Z', now, 'en')).toBe('2 days ago')
    expect(relativeTime('2026-10-21T11:59:40Z', now, 'en')).toBe('now')
  })
})

describe('pendingPeople', () => {
  const item = (over: Partial<QueueItem>): QueueItem => ({
    id: 'q1', method: 'POST', path: '/v1/shelters/s1/checkins', body: { data: { name: 'Ana', is_child: true } },
    volunteer_id: 'v', created_at: 5, attempts: 0, next_attempt_at: 0, state: 'pending', ...over,
  })
  it('lists the check-ins of this shelter still in the queue', () => {
    const rows = pendingPeople([item({}), item({ id: 'q2', path: '/v1/shelters/s2/checkins' }), item({ id: 'q3', path: '/v1/shelters/s1/exits' }), item({ id: 'q4', method: 'PUT' })], 's1')
    expect(rows).toEqual([{ queue_id: 'q1', name: 'Ana', social_name: undefined, wristband: undefined, is_child: true, created_at: 5, state: 'pending' }])
  })
})

describe('offline cache of the list', () => {
  const summary = (id: string): PersonSummary => ({ id, level: 'assisted', name: id, is_child: false, has_thumbnail: false, updated_at: '2026-10-21T10:00:00Z' })

  it('is replaced by the next list from the server and kept per shelter', async () => {
    const db = await freshDb()
    await cachePeople(db, 's1', [summary('a'), summary('b')])
    await cachePeople(db, 's2', [summary('z')])
    await cachePeople(db, 's1', [summary('b'), summary('c')])
    expect((await cachedPeople(db, 's1')).map((p) => p.id).sort()).toEqual(['b', 'c'])
    expect((await cachedPeople(db, 's2')).map((p) => p.id)).toEqual(['z'])
  })

  it('keeps the rest of the copy when the server cut the list (has_more)', async () => {
    const db = await freshDb()
    await cachePeople(db, 's1', [summary('a'), summary('b')])
    await cachePeople(db, 's1', [summary('b'), summary('c')], true)
    expect((await cachedPeople(db, 's1')).map((p) => p.id).sort()).toEqual(['a', 'b', 'c'])
  })

  it('does not erase a record made on this phone and not confirmed yet', async () => {
    const db = await freshDb()
    await db.put('people', { id: 'local', current_shelter_id: 's1', pending: true })
    await cachePeople(db, 's1', [summary('a')])
    expect(await db.get('people', 'local')).toBeDefined()
  })
})
