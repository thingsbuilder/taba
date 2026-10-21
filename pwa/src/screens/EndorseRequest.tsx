import { useEffect, useState } from 'preact/hooks'
import { countdown } from '../alerts'
import type { AppContext } from '../app-context'
import { errorKey } from '../app-context'
import { accuracyOk, getPosition } from '../geo'
import { t } from '../i18n'
import { QrBlock } from './QrBlock'

/** The new shelter's admin asks for an in-person endorsement code (spec 3.9 step 1): QR valid for 10 minutes. */
export function EndorseRequest({ ctx, shelterId }: { ctx: AppContext; shelterId: string }) {
  const [code, setCode] = useState<{ url: string; expires_at: string } | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [now, setNow] = useState(Date.now())
  useEffect(() => {
    if (!code) return
    const timer = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(timer)
  }, [code])

  async function request() {
    if (!ctx.active) return
    setBusy(true)
    setError(null)
    try {
      const position = await getPosition()
      if (!accuracyOk(position.accuracy)) return setError('insufficient_accuracy')
      const res = await ctx.api.createEndorsementCode(ctx.active.secret, shelterId, position)
      if (res.ok) setCode(res.data)
      else setError(res.error)
    } catch {
      setError('gps_failed')
    } finally {
      setBusy(false)
    }
  }

  const expired = code !== null && Date.parse(code.expires_at) <= now
  return (
    <section class="card">
      <h2 class="subtitle">{t('endorse_request.title')}</h2>
      <p class="body">{t('endorse_request.body')}</p>
      {error && <p class="notice notice-error" role="alert">{error === 'gps_failed' ? t('shelter_new.gps_failed') : t(errorKey(error))}</p>}
      {code && !expired && (
        <QrBlock url={code.url} label={t('endorse_request.qr_label')}>
          <p class="caption" role="timer">{t('endorse_request.left', { time: countdown(code.expires_at, now) })}</p>
        </QrBlock>
      )}
      {expired && <p class="notice" role="status">{t('endorse_request.expired')}</p>}
      <button type="button" class="button button-secondary" disabled={busy} onClick={() => void request()}>
        {code ? t('endorse_request.again') : t('endorse_request.action')}
      </button>
    </section>
  )
}
