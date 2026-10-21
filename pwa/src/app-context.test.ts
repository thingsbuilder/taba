import { describe, expect, it } from 'vitest'
import { errorKey } from './app-context'
import en from './i18n/en.json'
import ptBR from './i18n/pt-BR.json'

describe('errorKey', () => {
  it('maps the login error codes to their own message', () => {
    for (const code of ['banned', 'capture_inconclusive', 'capture_failed', 'capture_canceled', 'unico_busy', 'unico_limit', 'network']) {
      expect(errorKey(code)).toBe(`error.${code}`)
    }
  })

  it('falls back to the generic message for codes the app does not know', () => {
    expect(errorKey('unico_misconfigured')).toBe('error.generic')
    expect(errorKey('http_500')).toBe('error.generic')
  })

  it('every mapped message exists in both languages', () => {
    for (const code of ['banned', 'capture_inconclusive', 'capture_failed', 'capture_canceled', 'capture_expired', 'capture_consumed', 'capture_wrong_device', 'capture_not_found', 'unico_busy', 'unico_limit', 'unico_degraded', 'unico_unavailable', 'invalid_profile', 'network', 'x']) {
      const key = errorKey(code)
      expect(en, key).toHaveProperty([key])
      expect(ptBR, key).toHaveProperty([key])
    }
  })
})

describe('translations', () => {
  it('pt-BR and en have exactly the same keys', () => {
    expect(Object.keys(ptBR).sort()).toEqual(Object.keys(en).sort())
  })
})
