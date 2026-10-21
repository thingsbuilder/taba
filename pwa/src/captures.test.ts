import { describe, expect, it, vi } from 'vitest'
import type { Api, ApiResult } from './api'
import { draftKey, finishCapture, pollCapture, readDraft, returnPathOf, startCapture } from './captures'
import { freshDb } from './testing'

const fakeApi = (result: ApiResult<{ capture_id: string; link: string; expires_at: string }>) =>
  ({ createCapture: vi.fn().mockResolvedValue(result) }) as unknown as Api & { createCapture: ReturnType<typeof vi.fn> }

describe('startCapture', () => {
  it('creates the capture for the purpose, keeps where to return and the draft', async () => {
    const db = await freshDb()
    const api = fakeApi({ ok: true, data: { capture_id: 'c1', link: 'https://unico/x', expires_at: '' } })
    const res = await startCapture({ db, api, now: () => 7 }, 'secret', 'revalidation', { shelter_id: 's1', expected_person_id: 'g1' }, '/shelter/s1/exit?person=p1', { a: 1 })
    expect(res).toEqual({ ok: true, link: 'https://unico/x' })
    expect(api.createCapture).toHaveBeenCalledWith('revalidation', { secret: 'secret', shelter_id: 's1', expected_person_id: 'g1' })
    expect(await db.get('captures', 'c1')).toEqual({ capture_id: 'c1', created_at: 7, purpose: 'revalidation', return_to: '/shelter/s1/exit?person=p1' })
    expect(await readDraft(db, 'c1')).toEqual({ a: 1 })
  })

  it('keeps nothing when the server refuses', async () => {
    const db = await freshDb()
    const res = await startCapture({ db, api: fakeApi({ ok: false, status: 503, error: 'unico_degraded' }) }, 's', 'checkin', {}, '/x', {})
    expect(res).toEqual({ ok: false, error: 'unico_degraded' })
    expect(await db.getAll('captures')).toEqual([])
  })
})

describe('returnPathOf', () => {
  it('adds the capture to the saved path', async () => {
    const db = await freshDb()
    await db.put('captures', { capture_id: 'c1', created_at: 1, purpose: 'checkin', return_to: '/shelter/s1/checkin' })
    await db.put('captures', { capture_id: 'c2', created_at: 1, purpose: 'revalidation', return_to: '/shelter/s1/exit?person=p1' })
    expect(await returnPathOf(db, 'c1')).toBe('/shelter/s1/checkin?capture=c1')
    expect(await returnPathOf(db, 'c2')).toBe('/shelter/s1/exit?person=p1&capture=c2')
  })
  it('is null for a login, an invite, an unknown capture or a path that leaves the app', async () => {
    const db = await freshDb()
    await db.put('captures', { capture_id: 'login', created_at: 1 })
    await db.put('captures', { capture_id: 'inv', created_at: 1, purpose: 'invite', invite_code: 'k' })
    await db.put('captures', { capture_id: 'evil', created_at: 1, purpose: 'checkin', return_to: '//evil.example' })
    await db.put('captures', { capture_id: 'evil2', created_at: 1, purpose: 'checkin', return_to: 'https://evil.example' })
    for (const id of ['login', 'inv', 'nope', 'evil', 'evil2']) expect(await returnPathOf(db, id)).toBeNull()
  })
})

describe('finishCapture', () => {
  it('forgets the record and the draft', async () => {
    const db = await freshDb()
    await db.put('captures', { capture_id: 'c1', created_at: 1 })
    await db.put('config', { a: 1 }, draftKey('c1'))
    await finishCapture(db, 'c1')
    expect(await db.get('captures', 'c1')).toBeUndefined()
    expect(await readDraft(db, 'c1')).toBeNull()
  })
})

describe('pollCapture', () => {
  const pending: ApiResult<string> = { ok: false, status: 202, error: 'capture_pending' }

  it('waits 2 s, 4 s, 8 s ... while the capture is pending, then returns the result', async () => {
    const call = vi.fn<() => Promise<ApiResult<string>>>()
      .mockResolvedValueOnce(pending).mockResolvedValueOnce(pending).mockResolvedValueOnce(pending).mockResolvedValueOnce(pending)
      .mockResolvedValue({ ok: true, data: 'done' })
    const sleeps: number[] = []
    const res = await pollCapture(call, async (ms) => void sleeps.push(ms))
    expect(res).toEqual({ ok: true, data: 'done' })
    expect(sleeps).toEqual([2000, 4000, 8000, 8000])
  })

  it('stops at once on any other error', async () => {
    const sleep = vi.fn()
    const res = await pollCapture(async () => ({ ok: false, status: 422, error: 'no_face' }) as ApiResult<string>, sleep)
    expect(res).toEqual({ ok: false, error: 'no_face', status: 422 })
    expect(sleep).not.toHaveBeenCalled()
  })

  it('gives up after two minutes', async () => {
    const res = await pollCapture(async () => pending, async () => {})
    expect(res).toEqual({ ok: false, error: 'timeout', status: 0 })
  })
})
