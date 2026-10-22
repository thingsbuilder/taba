import { describe, expect, it } from 'vitest'
import { createApi } from './api'

// The client of every phase 5 route: method, path and the session.

describe('the phase 5 API client', () => {
  it('calls each route with its method and path', async () => {
    const calls: { method: string; url: string; auth: string | null }[] = []
    const fetchFn = (async (url: string, init: RequestInit) => {
      calls.push({ method: init.method!, url, auth: (init.headers as Record<string, string>).Authorization ?? null })
      return new Response('{}', { status: 200 })
    }) as unknown as typeof fetch
    const api = createApi(fetchFn, async () => 'dev')
    await api.putItems('s', 'sh1', { items: { cesta: { scope: 'per_household', period_days: 7, once_per_event: false, high_value: false } } })
    await api.postDelivery('s', 'sh1', { id: 'd1', card_code: 'ABCD2345', person_id: 'p1', item: 'cesta', quantity: 1, delivered_at: 'x' })
    await api.listDeliveries('s', 'sh1', 'in_review')
    await api.listDeliveries('s', 'sh1')
    await api.reviewDelivery('s', 'd1', { decision: 'reviewed_ok', note: '' })
    expect(calls.map((c) => `${c.method} ${c.url} ${c.auth ? 'session' : 'public'}`)).toEqual([
      'PUT /v1/shelters/sh1/items session',
      'POST /v1/shelters/sh1/deliveries session',
      'GET /v1/shelters/sh1/deliveries?status=in_review session',
      'GET /v1/shelters/sh1/deliveries session',
      'POST /v1/deliveries/d1/review session',
    ])
  })
})
