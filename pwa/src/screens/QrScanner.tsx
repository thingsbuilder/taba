import { useEffect, useRef, useState } from 'preact/hooks'
import { t } from '../i18n'
import { readQr } from '../qr'

const SCAN_EVERY_MS = 300

/** The rear camera, read for QR codes until one is found (spec 6.1). Closes the camera when it leaves. */
export function QrScanner({ onCode, onClose }: { onCode: (text: string) => void; onClose: () => void }) {
  const video = useRef<HTMLVideoElement>(null)
  const canvas = useRef<HTMLCanvasElement>(null)
  const [denied, setDenied] = useState(false)

  useEffect(() => {
    let stream: MediaStream | null = null
    let timer: ReturnType<typeof setInterval> | undefined
    let stopped = false
    navigator.mediaDevices
      ?.getUserMedia({ video: { facingMode: 'environment' } })
      .then(async (s) => {
        if (stopped || !video.current) return s.getTracks().forEach((track) => track.stop())
        stream = s
        video.current.srcObject = s
        await video.current.play()
        timer = setInterval(() => {
          if (!video.current || !canvas.current) return
          void readQr(video.current, canvas.current).then((text) => text && !stopped && onCode(text))
        }, SCAN_EVERY_MS)
      })
      .catch(() => setDenied(true))
    if (!navigator.mediaDevices) setDenied(true)
    return () => {
      stopped = true
      clearInterval(timer)
      stream?.getTracks().forEach((track) => track.stop())
    }
  }, [])

  return (
    <div class="card">
      {denied ? (
        <p class="notice notice-error" role="alert">{t('card.scan_denied')}</p>
      ) : (
        <>
          <p class="body">{t('card.scan_hint')}</p>
          <video ref={video} playsInline muted style={{ width: '100%', borderRadius: '12px' }} />
          <canvas ref={canvas} hidden />
        </>
      )}
      <button type="button" class="button button-secondary" onClick={onClose}>{t('card.scan_stop')}</button>
    </div>
  )
}
