import type { SwitchResult } from './sessions'

/** i18n key and params for a failed switch (spec 3.1.1). */
export function switchFailure(result: Exclude<SwitchResult, { ok: true }>): { key: string; params: Record<string, number> } {
  switch (result.reason) {
    case 'wrong_pin':
      return { key: 'switch.wrong', params: { n: result.remaining } }
    case 'locked':
      return { key: 'switch.locked', params: {} }
    case 'no_pin':
      return { key: 'switch.no_pin', params: {} }
    case 'no_session':
      return { key: 'switch.no_session', params: {} }
  }
}
