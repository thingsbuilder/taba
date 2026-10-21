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
    expect(result).toEqual({ ok: false, status: 202, error: 'capture_pending', detail: {} })
  })

  it('returns the stable error code of a failed call', async () => {
    expect(await api(async () => reply(422, { error: 'profile_required', detail: {} })).createSession('c1')).toEqual({
      ok: false,
      status: 422,
      error: 'profile_required',
      detail: {},
    })
    expect(await api(async () => reply(502, 'not json')).getMe('s')).toMatchObject({ ok: false, status: 502, error: 'http_502' })
  })

  it('reports a dead network as status 0', async () => {
    const result = await api(() => Promise.reject(new TypeError('Failed to fetch'))).getMe('s')
    expect(result).toEqual({ ok: false, status: 0, error: 'network' })
  })

  it('keeps the error detail (the distance of a too_far endorsement)', async () => {
    const result = await api(async () => reply(422, { error: 'too_far', detail: { distance_m: 150 } })).endorse('s', { code: 'c', endorser_shelter_id: 'e', lat: 1, lon: 2, accuracy: 3 })
    expect(result).toEqual({ ok: false, status: 422, error: 'too_far', detail: { distance_m: 150 } })
  })

  it('calls the phase 2 routes with the documented methods, paths and bodies', async () => {
    const fetchFn = vi.fn<typeof fetch>().mockImplementation(async () => reply(200, {}))
    const a = api(fetchFn)
    await a.createCapture('invite')
    await a.getShelter('s', 'sh')
    await a.patchShelter('s', 'sh', { capacity: null })
    await a.closeShelter('s', 'sh')
    await a.revokeShelter('s', 'sh', 'why')
    await a.createInvite('s', 'sh', { kind: 'link', role: 'volunteer' })
    await a.acceptInvite('code', 'cap', { name: 'Ana', show_phone: false, language: 'en' })
    await a.promoteMember('s', 'sh', 'v1')
    await a.removeMember('s', 'sh', 'v1')
    await a.leaveShelter('s', 'sh')
    await a.resolveAlert('s', 'al')
    const calls = fetchFn.mock.calls.map(([path, init]) => [init?.method, path, init?.body])
    expect(calls).toEqual([
      ['POST', '/v1/captures', '{"purpose":"invite"}'],
      ['GET', '/v1/shelters/sh', undefined],
      ['PATCH', '/v1/shelters/sh', '{"capacity":null}'],
      ['POST', '/v1/shelters/sh/close', undefined],
      ['POST', '/v1/shelters/sh/revoke', '{"reason":"why"}'],
      ['POST', '/v1/shelters/sh/invites', '{"kind":"link","role":"volunteer"}'],
      ['POST', '/v1/invites/code/accept', '{"capture_id":"cap","profile":{"name":"Ana","show_phone":false,"language":"en"}}'],
      ['PATCH', '/v1/shelters/sh/members/v1', '{"role":"admin"}'],
      ['DELETE', '/v1/shelters/sh/members/v1', undefined],
      ['POST', '/v1/shelters/sh/leave', undefined],
      ['POST', '/v1/alerts/al/resolve', undefined],
    ])
  })
})
