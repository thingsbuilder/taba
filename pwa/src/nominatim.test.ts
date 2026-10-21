import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { Db } from './db'
import { createNominatim, parseAddress } from './nominatim'
import { createQueue } from './queue'
import { freshDb } from './testing'

const reply = {
  display_name: 'Rua A, Porto Alegre, RS, Brasil',
  address: { city: 'Porto Alegre', 'ISO3166-2-lvl4': 'BR-RS' },
}
const ok = (body: unknown = reply) => new Response(JSON.stringify(body), { status: 200 })

describe('parseAddress', () => {
  it('takes the city from the first of city, town, village, municipality and the UF from the ISO code', () => {
    expect(parseAddress(reply)).toEqual({ text: reply.display_name, city: 'Porto Alegre', state: 'RS' })
    expect(parseAddress({ display_name: 'x', address: { town: 'T', village: 'V', 'ISO3166-2-lvl4': 'BR-SC' } })?.city).toBe('T')
    expect(parseAddress({ display_name: 'x', address: { village: 'V' } })?.city).toBe('V')
    expect(parseAddress({ display_name: 'x', address: { municipality: 'M' } })?.city).toBe('M')
  })
  it('leaves city and state empty when the reply has none, and rejects a reply without text', () => {
    expect(parseAddress({ display_name: 'x' })).toEqual({ text: 'x', city: '', state: '' })
    expect(parseAddress({ display_name: 'x', address: { 'ISO3166-2-lvl4': 'BRRS' } })?.state).toBe('')
    expect(parseAddress({})).toBeNull()
  })
})

describe('createNominatim', () => {
  let db: Db
  let clock: number
  const fetchMock = vi.fn<typeof fetch>()
  const sleeps: number[] = []
  const queueFetch = vi.fn<typeof fetch>()

  function make() {
    const queue = createQueue({ db, fetch: queueFetch, deviceId: async () => 'd', getStatus: async () => ({ unico: 'ok' }), now: () => clock })
    const nominatim = createNominatim({
      db, queue, fetch: fetchMock, now: () => clock,
      sleep: async (ms) => { sleeps.push(ms); clock += ms },
    })
    return { queue, nominatim }
  }

  beforeEach(async () => {
    db = await freshDb()
    clock = 1_000_000
    fetchMock.mockReset()
    queueFetch.mockReset().mockResolvedValue(new Response('{}', { status: 500 })) // keep queued items visible
    sleeps.length = 0
  })

  it('asks Nominatim for the coordinates in pt-BR and caches the answer by 4-decimal coordinates', async () => {
    fetchMock.mockResolvedValue(ok())
    const { nominatim } = make()
    expect(await nominatim.reverse(-30.03451, -51.2301)).toMatchObject({ city: 'Porto Alegre', state: 'RS' })
    expect(fetchMock.mock.calls[0]![0]).toBe(
      'https://nominatim.openstreetmap.org/reverse?format=jsonv2&lat=-30.03451&lon=-51.2301&accept-language=pt-BR')
    await nominatim.reverse(-30.03453, -51.23012) // same cell after rounding
    expect(fetchMock).toHaveBeenCalledTimes(1)
  })

  it('never sends two requests less than one second apart', async () => {
    fetchMock.mockResolvedValue(ok())
    const { nominatim } = make()
    await Promise.all([nominatim.reverse(1, 1), nominatim.reverse(2, 2), nominatim.reverse(3, 3)])
    expect(fetchMock).toHaveBeenCalledTimes(3)
    expect(sleeps).toEqual([1000, 1000])
  })

  it('returns null on a network failure or an error status, and caches nothing', async () => {
    const { nominatim } = make()
    fetchMock.mockRejectedValueOnce(new Error('offline')).mockResolvedValueOnce(new Response('', { status: 429 }))
    expect(await nominatim.reverse(1, 1)).toBeNull()
    expect(await nominatim.reverse(1, 1)).toBeNull()
    expect(await db.get('config', 'nominatim:1.0000,1.0000')).toBeUndefined()
  })

  it('resolveFor queues a PATCH for the shelter when the address arrives', async () => {
    fetchMock.mockResolvedValue(ok())
    const { nominatim, queue } = make()
    await nominatim.resolveFor('s1', 1, 1, 'ana')
    const [item] = await queue.list()
    expect(item).toMatchObject({
      method: 'PATCH', path: '/v1/shelters/s1', shelter_id: 's1', volunteer_id: 'ana',
      body: { address: { text: reply.display_name, city: 'Porto Alegre', state: 'RS' } },
    })
    expect(await db.count('addresses')).toBe(0)
  })

  it('without signal keeps the coordinates in `addresses` and retryPending finishes the job later', async () => {
    const { nominatim, queue } = make()
    fetchMock.mockRejectedValueOnce(new Error('offline'))
    await nominatim.resolveFor('s1', 1, 1, 'ana')
    expect(await db.get('addresses', 's1')).toEqual({ shelter_id: 's1', lat: 1, lon: 1, volunteer_id: 'ana' })
    expect(await queue.count()).toBe(0)

    fetchMock.mockRejectedValueOnce(new Error('still offline'))
    await nominatim.retryPending()
    expect(await db.count('addresses')).toBe(1)

    fetchMock.mockResolvedValue(ok())
    await nominatim.retryPending()
    expect(await db.count('addresses')).toBe(0)
    expect((await queue.list())[0]).toMatchObject({ path: '/v1/shelters/s1', volunteer_id: 'ana' })
  })
})
