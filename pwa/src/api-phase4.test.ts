import { describe, expect, it } from 'vitest'
import { createApi } from './api'

// The client of every phase 4 route: method, path and who is asked for a session.

describe('the phase 4 API client', () => {
  const calls: { method: string; url: string; auth: string | null; body: unknown }[] = []
  const fetchFn = (async (url: string, init: RequestInit) => {
    const headers = init.headers as Record<string, string>
    calls.push({ method: init.method!, url, auth: headers.Authorization ?? null, body: init.body ? JSON.parse(init.body as string) : undefined })
    return new Response('{}', { status: 200 })
  }) as unknown as typeof fetch
  const api = createApi(fetchFn, async () => 'dev')

  it('calls each route with its method and path; the family link has two public calls', async () => {
    const photo = { photos_base64: ['AAAA'], missing_person: { name: 'Ana', notes: '' }, family: { name: 'Rui', phone: '5511999999999', relationship: 'son', language: 'pt-BR' as const } }
    await api.postRescue('s', { id: 'r1', lat: 1, lon: 2, accuracy: 3, captured_at: 'x' })
    await api.patchRescue('s', 'r1', { landing_point: 'Porto' })
    await api.nearbyShelters('s', -30.1, -51.2)
    await api.postSearch('s', { id: 's1', ...photo })
    await api.listMatches('s')
    await api.getMatch('s', 'm1')
    await api.decideMatch('s', 'm1', { decision: 'accept', decided_by: 'person' })
    await api.grantRole('s', 'v1', 'mediator')
    await api.revokeRole('s', 'v1', 'protection')
    await api.createSearchLink('s', { shelter_id: 'sh1' })
    await api.cancelSearchLink('s', 'c1')
    await api.getSearchLink('c1')
    await api.submitSearchLink('c1', photo)
    expect(calls.map((c) => `${c.method} ${c.url} ${c.auth ? 'session' : 'public'}`)).toEqual([
      'POST /v1/rescues session',
      'PATCH /v1/rescues/r1 session',
      'GET /v1/shelters/nearby?lat=-30.1&lon=-51.2 session',
      'POST /v1/searches session',
      'GET /v1/matches session',
      'GET /v1/matches/m1 session',
      'POST /v1/matches/m1/decision session',
      'POST /v1/volunteers/v1/roles session',
      'DELETE /v1/volunteers/v1/roles/protection session',
      'POST /v1/search-links session',
      'DELETE /v1/search-links/c1 session',
      'GET /v1/search-links/c1 public',
      'POST /v1/search-links/c1/submit public',
    ])
    expect(calls[7]!.body).toEqual({ role: 'mediator' })
  })

  it('reads the match detail as the body itself, as the API sends it', async () => {
    const detail = { id: 'm1', status: 'open' }
    const res = await createApi((async () => new Response(JSON.stringify(detail), { status: 200 })) as unknown as typeof fetch, async () => 'dev').getMatch('s', 'm1')
    expect(res.ok && res.data).toEqual(detail)
  })
})
