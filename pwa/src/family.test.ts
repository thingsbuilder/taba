import { describe, expect, it, vi } from 'vitest'
import en from './i18n/en.json'
import es from './i18n/es.json'
import ptBR from './i18n/pt-BR.json'
import { buildRequest, checkLink, defaultFamilyLang, emptyForm, ft, submitSearch, validate } from './family'

const json = (status: number, body: unknown = {}) => new Response(JSON.stringify(body), { status })
const filled = { ...emptyForm('es'), missing_name: ' Rui ', family_name: 'Ana', family_phone: ' 11999990000 ', family_relationship: 'filha' }

describe('family page language', () => {
  it('follows the browser: pt, es, otherwise en', () => {
    expect(defaultFamilyLang(['pt-BR'])).toBe('pt-BR')
    expect(defaultFamilyLang(['es-AR', 'en'])).toBe('es')
    expect(defaultFamilyLang(['fr-FR'])).toBe('en')
    expect(defaultFamilyLang([])).toBe('en')
  })
  it('has the same keys in PT, EN and ES, and the fixed notice and the answer word for word', () => {
    const keys = (d: Record<string, string>) => Object.keys(d).filter((k) => k.startsWith('family.')).sort()
    expect(keys(en)).toEqual(keys(ptBR))
    expect(keys(es)).toEqual(keys(ptBR))
    expect(ft('pt-BR', 'notice')).toBe('Esta busca não substitui o registro de desaparecimento na delegacia.')
    expect(ft('pt-BR', 'done_message')).toBe('Busca registrada. Se a pessoa for encontrada e concordar, a equipe entrará em contato com a família.')
    expect(ft('pt-BR', 'invalid')).toBe('Este link expirou ou já foi usado. Peça outro a quem enviou.')
    expect(ft('pt-BR', 'error_degraded')).toBe('O reconhecimento facial está indisponível agora. Tente mais tarde.')
    expect(ft('pt-BR', 'error_offline')).toBe('Sem sinal. Tente de novo.')
  })
  it('says in the consent what is kept, who sees it and that police and immigration never get it', () => {
    expect(ft('pt-BR', 'consent_stored')).toContain('só até a equipe conferir o rosto')
    expect(ft('pt-BR', 'consent_no_police')).toMatch(/polícia.*imigração/)
    expect(ft('en', 'consent_no_police')).toMatch(/police.*immigration/)
    expect(ft('es', 'consent_no_police')).toMatch(/policía.*inmigración/)
  })
})

describe('validate', () => {
  it('asks for the required fields, 1 to 5 photos and the consent', () => {
    expect(validate(filled, 2, true)).toEqual([])
    expect(validate(emptyForm('en'), 0, false)).toEqual(['missing_name', 'family_name', 'family_phone', 'family_relationship', 'photos', 'consent'])
    expect(validate(filled, 6, true)).toEqual(['photos'])
    expect(validate({ ...filled, family_phone: '   ' }, 1, true)).toEqual(['family_phone'])
  })
})

describe('buildRequest', () => {
  it('trims, drops an empty nickname and carries the family language', () => {
    expect(buildRequest(filled, ['a'])).toEqual({
      photos_base64: ['a'],
      missing_person: { name: 'Rui', notes: '' },
      family: { name: 'Ana', phone: '11999990000', relationship: 'filha', language: 'es' },
    })
    expect(buildRequest({ ...filled, missing_nickname: ' Ruizinho ' }, []).missing_person.nickname).toBe('Ruizinho')
  })
})

describe('checkLink', () => {
  it('reads valid, a 410 and a failure apart, without a session header', async () => {
    const f = vi.fn<typeof fetch>().mockResolvedValueOnce(json(200, { valid: true }))
    expect(await checkLink('abc', f)).toBe('valid')
    expect(f.mock.calls[0]![0]).toBe('/v1/search-links/abc')
    expect(f.mock.calls[0]![1]?.headers).toBeUndefined()
    expect(await checkLink('abc', vi.fn<typeof fetch>().mockResolvedValue(json(410, { error: 'search_link_invalid' })))).toBe('invalid')
    expect(await checkLink('abc', vi.fn<typeof fetch>().mockResolvedValue(json(200, { valid: false })))).toBe('invalid')
    expect(await checkLink('abc', vi.fn<typeof fetch>().mockRejectedValue(new TypeError('x')))).toBe('network')
    expect(await checkLink('abc', vi.fn<typeof fetch>().mockResolvedValue(json(500)))).toBe('network')
  })
})

describe('submitSearch', () => {
  const send = (res: Response | Error) => submitSearch('abc', buildRequest(filled, ['a']), vi.fn<typeof fetch>()[res instanceof Error ? 'mockRejectedValue' : 'mockResolvedValue'](res as never))
  it('posts to the submit route with no Authorization header and returns only usable_photos', async () => {
    const f = vi.fn<typeof fetch>().mockResolvedValue(json(202, { message: 'search_registered', usable_photos: 2 }))
    expect(await submitSearch('abc', buildRequest(filled, ['a']), f)).toEqual({ kind: 'done', usable: 2 })
    const [path, init] = f.mock.calls[0]!
    expect(path).toBe('/v1/search-links/abc/submit')
    expect(init?.method).toBe('POST')
    expect(init?.headers).toEqual({ 'Content-Type': 'application/json' })
  })
  it('maps each failure to its own text; 503 unico_degraded keeps the link', async () => {
    expect(await send(new TypeError('x'))).toEqual({ kind: 'error', key: 'error_offline' })
    expect(await send(json(503, { error: 'unico_degraded' }))).toEqual({ kind: 'error', key: 'error_degraded' })
    expect(await send(json(413, { error: 'image_too_large' }))).toEqual({ kind: 'error', key: 'error_too_big' })
    expect(await send(json(410, { error: 'search_link_invalid' }))).toEqual({ kind: 'invalid' })
    expect(await send(json(429, { error: 'rate_limited' }))).toEqual({ kind: 'error', key: 'error_busy' })
    expect(await send(json(400, { error: 'invalid_request' }))).toEqual({ kind: 'error', key: 'error_check' })
    expect(await send(json(500))).toEqual({ kind: 'error', key: 'error_generic' })
  })
  it('an answer with no usable photo is not a success: the link still works and the family sends others', async () => {
    expect(await send(json(202, { message: 'search_registered', usable_photos: 0 }))).toEqual({ kind: 'none_usable' })
    expect(await send(json(202, { message: 'search_registered', usable_photos: 1 }))).toEqual({ kind: 'done', usable: 1 })
  })
  it('tries again at once when the server is busy with the link, and gives up after a few', async () => {
    const waits: number[] = []
    const sleep = async (ms: number) => void waits.push(ms)
    const busy = () => json(503, { error: 'contention' })
    const flaky = vi.fn<typeof fetch>().mockImplementationOnce(async () => busy()).mockImplementationOnce(async () => busy()).mockImplementation(async () => json(202, { message: 'search_registered', usable_photos: 2 }))
    expect(await submitSearch('abc', buildRequest(filled, ['a']), flaky, sleep)).toEqual({ kind: 'done', usable: 2 })
    expect(flaky).toHaveBeenCalledTimes(3)
    expect(waits).toEqual([1000, 1000])
    const stuck = vi.fn<typeof fetch>().mockImplementation(async () => busy())
    expect(await submitSearch('abc', buildRequest(filled, ['a']), stuck, sleep)).toEqual({ kind: 'error', key: 'error_contention' })
    expect(stuck).toHaveBeenCalledTimes(4)
  })
})
