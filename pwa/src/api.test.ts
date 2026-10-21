import { describe, expect, it, vi } from 'vitest'
import { createApi } from './api'

const reply = (status: number, body?: unknown) => new Response(body === undefined ? null : JSON.stringify(body), { status })

function api(fetchFn: typeof fetch) {
  return createApi(fetchFn, async () => 'device-1')
}

describe('createApi', () => {
  it('sends the device id on every call and the session as a Bearer token when there is one', async () => {
    const fetchFn = vi.fn<typeof fetch>().mockResolvedValue(reply(200, {}))
    await api(fetchFn).getMe('the-secret')
    await api(fetchFn).createCapture()
    const [[, me], [, capture]] = fetchFn.mock.calls
    expect(me?.headers).toMatchObject({ 'X-Device-Id': 'device-1', Authorization: 'Bearer the-secret' })
    expect(capture?.headers).toMatchObject({ 'X-Device-Id': 'device-1', 'Content-Type': 'application/json' })
    expect((capture?.headers as Record<string, string>).Authorization).toBeUndefined()
    expect(capture?.body).toBe(JSON.stringify({ purpose: 'login' }))
  })

  it('posts the capture and, only when asked, the profile', async () => {
    const fetchFn = vi.fn<typeof fetch>().mockResolvedValue(reply(201, { session: 's' }))
    await api(fetchFn).createSession('c1')
    await api(fetchFn).createSession('c1', { name: 'Ana', show_phone: false, language: 'en' })
    expect(JSON.parse(fetchFn.mock.calls[0]![1]!.body as string)).toEqual({ capture_id: 'c1' })
    expect(JSON.parse(fetchFn.mock.calls[1]![1]!.body as string)).toEqual({
      capture_id: 'c1',
      profile: { name: 'Ana', show_phone: false, language: 'en' },
    })
  })

  it('returns the body on success and nothing for 204', async () => {
    expect(await api(async () => reply(201, { session: 's' })).createSession('c1')).toEqual({ ok: true, data: { session: 's' } })
    expect(await api(async () => reply(204)).deleteSession('s')).toEqual({ ok: true, data: null })
  })

  it('202 is "capture_pending", not a result', async () => {
    const result = await api(async () => reply(202, { error: 'capture_pending', detail: {} })).createSession('c1')
    expect(result).toEqual({ ok: false, status: 202, error: 'capture_pending' })
  })

  it('returns the stable error code of a failed call', async () => {
    expect(await api(async () => reply(422, { error: 'profile_required', detail: {} })).createSession('c1')).toEqual({
      ok: false,
      status: 422,
      error: 'profile_required',
    })
    expect(await api(async () => reply(502, 'not json')).getMe('s')).toMatchObject({ ok: false, status: 502, error: 'http_502' })
  })

  it('reports a dead network as status 0', async () => {
    const result = await api(() => Promise.reject(new TypeError('Failed to fetch'))).getMe('s')
    expect(result).toEqual({ ok: false, status: 0, error: 'network' })
  })
})
