import { useEffect, useState } from 'preact/hooks'
import { qrSvg } from '../qr'
import { t } from '../i18n'

/** QR code, the URL in mono type and copy/share (spec 3.3: the QR and the link carry the same URL). */
export function QrBlock({ url, label, children }: { url: string; label: string; children?: preact.ComponentChildren }) {
  const [svg, setSvg] = useState('')
  const [copied, setCopied] = useState(false)
  useEffect(() => {
    void qrSvg(url).then(setSvg)
  }, [url])

  const canShare = 'share' in navigator
  async function share() {
    if (canShare) await navigator.share({ url }).catch(() => undefined)
    else {
      await navigator.clipboard?.writeText(url).catch(() => undefined)
      setCopied(true)
    }
  }

  return (
    <div class="card qr-block">
      <div class="qr" role="img" aria-label={label} dangerouslySetInnerHTML={{ __html: svg }} />
      <p class="code">{url}</p>
      {children}
      <button type="button" class="button button-secondary" onClick={() => void share()}>
        {canShare ? t('qr.share') : t('qr.copy')}
      </button>
      {copied && <p class="notice" role="status">{t('qr.copied')}</p>}
    </div>
  )
}
