import { describe, expect, it } from 'vitest'
import { createApi } from './api'

// The client of every phase 6 route: method, path and the session.

describe('the phase 6 API client', () => {
  it('calls each route with its method and path; the two CSVs return the raw response', async () => {
    const calls: { method: string; url: string; auth: string | null }[] = []
    const fetchFn = (async (url: string, init: RequestInit) => {
      const headers = (init.headers ?? {}) as Record<string, string>
      calls.push({ method: init.method ?? 'GET', url, auth: headers.Authorization ?? null })
      return new Response('{}', { status: 200 })
    }) as unknown as typeof fetch
    const api = createApi(fetchFn, async () => 'dev')
    await api.getDashboardEvents('s')
    await api.getDashboardEvent('s', 'e1')
    expect((await api.getDailyCsv('s', 'e1')).status).toBe(200)
    expect((await api.getShelterExport('s', 'sh1')).status).toBe(200)
    expect(calls.map((c) => `${c.method} ${c.url} ${c.auth ? 'session' : 'public'}`)).toEqual([
      'GET /v1/dashboard/events session',
      'GET /v1/dashboard/events/e1 session',
      'GET /v1/dashboard/events/e1/daily.csv session',
      'GET /v1/shelters/sh1/export.csv session',
    ])
  })
})
