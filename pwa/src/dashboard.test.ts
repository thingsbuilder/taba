import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { AlertView } from './api'
import { anomaliesOf, canSeeDashboard, formatGeneratedAt, formatNumber, formatRatio, saveCsv } from './dashboard'
import { setLanguage, t } from './i18n'

const alert = (kind: string, over: Partial<AlertView> = {}): AlertView => ({
  id: kind, kind, informational: true, target_kind: 'shelter', target_id: 's', event_id: 'e1', shelter_ids: [], volunteer_ids: [], detail: {}, created_at: '', resolved: false, ...over,
})

describe('dashboard helpers', () => {
  beforeEach(() => setLanguage('pt-BR'))

  it('only a validator or the Civil Defense sees the panel', () => {
    expect(canSeeDashboard(['validator'])).toBe(true)
    expect(canSeeDashboard(['civil_defense'])).toBe(true)
    expect(canSeeDashboard(['mediator', 'protection'])).toBe(false)
    expect(canSeeDashboard([])).toBe(false)
  })

  it('shows the time the panel was built in the language of the app', () => {
    const iso = '2026-10-22T10:10:56.226964Z'
    expect(formatGeneratedAt(iso)).toBe(new Date(iso).toLocaleString('pt-BR'))
    expect(t('dashboard.updated_at', { time: '10:10' })).toBe('Atualizado em 10:10')
    for (const k of ['shelters', 'exits', 'pets']) expect(t(`dashboard.partial.${k}`)).not.toContain('dashboard.partial')
  })

  it('formats numbers in the language of the app', () => {
    expect(formatNumber(1234567)).toBe('1.234.567')
    expect(formatRatio(0.125)).toBe('12,5%')
    expect(formatRatio(null)).toBe('–')
    setLanguage('en')
    expect(formatNumber(1234567)).toBe('1,234,567')
    expect(formatRatio(0.125)).toBe('12.5%')
  })

  it('labels the household and animal cuts as those created at the shelter, and the Unico ratio as an estimate', () => {
    expect(t('dashboard.households')).toContain('criados neste abrigo')
    expect(t('dashboard.pets')).toContain('criados neste abrigo')
    expect(t('dashboard.unico_per_person')).toContain('estimativa')
    setLanguage('en')
    expect(t('dashboard.households')).toContain('created at this shelter')
    expect(t('dashboard.pets')).toContain('created at this shelter')
    expect(t('dashboard.unico_per_person')).toContain('estimate')
  })

  it('keeps only open anomaly alerts of the event', () => {
    const list = [alert('anomaly_volume'), alert('out_of_area', { event_id: null }), alert('anomaly_distance', { event_id: 'e2' }), alert('anomaly_volume', { resolved: true }), alert('shelter_flagged')]
    expect(anomaliesOf(list, 'e1').map((a) => a.kind)).toEqual(['anomaly_volume', 'out_of_area'])
  })

  it('saves the CSV as a file and refuses an error response', async () => {
    const click = vi.fn()
    const anchor = { click, remove: vi.fn(), href: '', download: '' }
    vi.stubGlobal('document', { createElement: () => anchor, body: { appendChild: vi.fn() } })
    URL.createObjectURL = () => 'blob:x'
    URL.revokeObjectURL = vi.fn()
    expect(await saveCsv(new Response('a,b', { status: 200 }), 'daily.csv')).toBe(true)
    expect(anchor.download).toBe('daily.csv')
    expect(click).toHaveBeenCalledOnce()
    expect(await saveCsv(new Response('', { status: 403 }), 'daily.csv')).toBe(false)
    vi.unstubAllGlobals()
  })
})
