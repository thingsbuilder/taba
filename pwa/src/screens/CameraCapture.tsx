import { useEffect, useRef, useState } from 'preact/hooks'
import { type Facing, cameraErrorKey, frameProblem, grabFrame, openCamera, sampleFrame, stopStream, videoReady, waitForFirstFrame } from '../camera'
import type { Frame } from '../image'
import { t } from '../i18n'

/** The app camera (spec 6.1.3): live video, "take", then confirm or retake. No file picker, so no gallery. */
export function CameraCapture({ facing: initial = 'environment', onPhoto, onCancel }: {
  facing?: Facing
  onPhoto: (frame: Frame) => void
  onCancel: () => void
}) {
  const video = useRef<HTMLVideoElement>(null)
  const [facing, setFacing] = useState<Facing>(initial)
  const [error, setError] = useState<string | null>(null)
  const [ready, setReady] = useState(false)
  const [notice, setNotice] = useState<string | null>(null)
  const [shot, setShot] = useState<{ frame: Frame; url: string } | null>(null)

  useEffect(() => {
    if (shot) return
    let stream: MediaStream | null = null
    let alive = true
    setReady(false)
    openCamera(facing).then(
      (s) => {
        if (!alive) return stopStream(s)
        stream = s
        const el = video.current
        if (el) {
          el.srcObject = s
          // Ready only with a first real frame: the rear camera of an iPhone plays before it has a picture.
          void el.play().then(() => waitForFirstFrame(el)).then((frame) => alive && (frame ? setReady(true) : setError('checkin.camera_bad_frame')))
        }
      },
      (e) => alive && setError(cameraErrorKey(e)),
    )
    return () => {
      alive = false
      stopStream(stream)
    }
  }, [facing, shot === null])

  async function take() {
    const el = video.current
    if (!el) return
    setNotice(null)
    if (!videoReady(el)) return setNotice('checkin.camera_bad_frame')
    const frame = grabFrame(el)
    // Nothing is offered that Unico would only refuse: no size, too small, or a black or flat picture.
    if (frameProblem(frame, sampleFrame(frame))) return setNotice('checkin.camera_bad_frame')
    const blob = await new Promise<Blob | null>((r) => (frame.source as HTMLCanvasElement).toBlob(r, 'image/jpeg', 0.5))
    if (blob) setShot({ frame, url: URL.createObjectURL(blob) })
  }

  function retake() {
    if (shot) URL.revokeObjectURL(shot.url)
    setShot(null)
  }

  if (error) {
    return (
      <section class="card">
        <p class="notice notice-error" role="alert">{t(error)}</p>
        <button type="button" class="button button-secondary" onClick={onCancel}>{t('checkin.camera_cancel')}</button>
      </section>
    )
  }
  return (
    <section class="card">
      {shot ? (
        <img class="camera-view" src={shot.url} alt={t('checkin.camera_preview')} />
      ) : (
        <>
          <video class="camera-view" ref={video} playsInline muted aria-label={t('checkin.camera_label')} />
          {!ready && <p class="caption" role="status">{t('checkin.camera_opening')}</p>}
          <p class="caption">{t('checkin.camera_hint')}</p>
          {notice && <p class="notice notice-warning" role="alert">{t(notice)}</p>}
        </>
      )}
      {shot ? (
        <>
          <button type="button" class="button" onClick={() => { URL.revokeObjectURL(shot.url); onPhoto(shot.frame) }}>{t('checkin.camera_use')}</button>
          <button type="button" class="button button-secondary" onClick={retake}>{t('checkin.camera_retake')}</button>
        </>
      ) : (
        <>
          <button type="button" class="button" disabled={!ready} onClick={() => void take()}>{t('checkin.camera_take')}</button>
          <button type="button" class="button button-secondary" onClick={() => setFacing(facing === 'user' ? 'environment' : 'user')}>{t('checkin.camera_flip')}</button>
          <button type="button" class="button button-secondary" onClick={onCancel}>{t('checkin.camera_cancel')}</button>
        </>
      )}
    </section>
  )
}
