import { describe, expect, it, vi } from 'vitest'
import type { SessionResponse } from './api'
import { getLanguage, loadLanguage, setLanguage } from './i18n'
import { chooseLanguage } from './language'
import { finishLogin } from './login'
import { createQueue } from './queue'
import { addLoginSession, getActive } from './sessions'
import { freshDb } from './testing'

// Criterion 58: the choice survives a reload and, signed in, follows the profile to other phones.

const response = (language: 'pt-BR' | 'en'): SessionResponse => ({
  session: 'secret-ana', expires_at: new Date(Date.now() + 7 * 86_400_000).toISOString(),
  volunteer: { id: 'ana', name: 'Ana', phone: null, show_phone: false, language },
})

describe('choosing the language', () => {
  it('signed out: it is kept on the phone and wins after a reload', async () => {
    const db = await freshDb()
    const queue = createQueue({ db, fetch: vi.fn<typeof fetch>(), deviceId: async () => 'd', getStatus: async () => null })
    setLanguage('pt-BR')
    await chooseLanguage(db, queue, null, 'en')
    expect(getLanguage()).toBe('en')
    expect(await loadLanguage(db, ['pt-BR'])).toBe('en') // a reload, on a phone that says Portuguese
    expect(await queue.count()).toBe(0) // no profile to update
  })

  it('signed in: the profile follows through the queue, even offline, and the session copy changes', async () => {
    const db = await freshDb()
    const offline = vi.fn<typeof fetch>().mockRejectedValue(new TypeError('offline'))
    const queue = createQueue({ db, fetch: offline, deviceId: async () => 'd', getStatus: async () => null })
    const session = await addLoginSession(db, response('pt-BR'), Date.now())
    await chooseLanguage(db, queue, session, 'en')
    const [item] = await queue.list()
    expect(item).toMatchObject({ method: 'PATCH', path: '/v1/me', body: { language: 'en' }, volunteer_id: 'ana' })
    expect((await getActive(db, Date.now()))?.profile.language).toBe('en')
    expect(await loadLanguage(db, ['pt-BR'])).toBe('en')
  })

  it('on another phone the language of the profile wins at login, over what that phone had', async () => {
    const phone = await freshDb() // never chose anything: it would follow the browser, Portuguese
    setLanguage('pt-BR')
    const api = { createSession: async () => ({ ok: true as const, data: response('en') }) }
    const outcome = await finishLogin({ db: phone, api: api as never, sleep: async () => {} }, 'c1')
    expect(outcome.kind).toBe('logged_in')
    expect(getLanguage()).toBe('en')
    expect(await loadLanguage(phone, ['pt-BR'])).toBe('en')
  })
})
