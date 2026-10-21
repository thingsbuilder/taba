import type { Frame } from './image'

// The app camera (spec 6.1.3): getUserMedia plus a canvas. No file input, so the gallery is never offered.

export type Facing = 'user' | 'environment'

export const cameraSupported = (nav: { mediaDevices?: Pick<MediaDevices, 'getUserMedia'> }): boolean =>
  typeof nav.mediaDevices?.getUserMedia === 'function'

/** i18n key of what to tell the volunteer when the camera does not open. Never blames the person. */
export function cameraErrorKey(error: unknown): string {
  const name = (error as { name?: string } | null)?.name
  if (name === 'NotAllowedError' || name === 'SecurityError') return 'checkin.camera_denied'
  if (name === 'NotReadableError' || name === 'AbortError') return 'checkin.camera_busy'
  return 'checkin.camera_unsupported'
}

/** Without a size the phone camera opens at 480x640, which Unico refused (INVALID_REQUEST); ask for the best the camera has up to full HD. */
export const CAMERA_CONSTRAINTS = { width: { ideal: 1920 }, height: { ideal: 1080 } } as const

export function openCamera(facing: Facing, nav: { mediaDevices?: Pick<MediaDevices, 'getUserMedia'> } = navigator): Promise<MediaStream> {
  if (!cameraSupported(nav)) return Promise.reject(new DOMException('no camera', 'NotSupportedError'))
  return nav.mediaDevices!.getUserMedia({ video: { facingMode: facing, ...CAMERA_CONSTRAINTS }, audio: false })
}

export const stopStream = (stream: MediaStream | null): void => stream?.getTracks().forEach((t) => t.stop())

/** A frame smaller than this on its larger side is of no use to the face check. */
export const MIN_FRAME_SIDE = 320
/** Mean brightness (0..255) under which the picture is "all black", and the spread of brightness under which it is uniform. */
const BLACK_MEAN = 8
const UNIFORM_SPREAD = 4
/** Pixels are sampled from a picture this many pixels wide, never from the full frame. */
const SAMPLE_SIDE = 32

export type FrameProblem = 'empty' | 'too_small' | 'blank'

/**
 * Whether a frame is worth sending: it has a size, at least `MIN_FRAME_SIDE` on its larger side, and is
 * not (nearly) all black or one flat colour. `pixels` is RGBA of a small copy of the frame (`sampleFrame`);
 * without it only the size is checked.
 */
export function frameProblem(size: { width: number; height: number }, pixels?: Uint8ClampedArray): FrameProblem | null {
  if (!(size.width > 0 && size.height > 0)) return 'empty'
  if (Math.max(size.width, size.height) < MIN_FRAME_SIDE) return 'too_small'
  if (!pixels || pixels.length < 4) return null
  let sum = 0
  let sumSquares = 0
  const count = Math.floor(pixels.length / 4)
  for (let i = 0; i < count; i++) {
    const luma = 0.299 * pixels[i * 4]! + 0.587 * pixels[i * 4 + 1]! + 0.114 * pixels[i * 4 + 2]!
    sum += luma
    sumSquares += luma * luma
  }
  const mean = sum / count
  const spread = Math.sqrt(Math.max(0, sumSquares / count - mean * mean))
  return mean < BLACK_MEAN || spread < UNIFORM_SPREAD ? 'blank' : null
}

/** RGBA of a 32x32 copy of the frame. */
export function sampleFrame(frame: Frame): Uint8ClampedArray {
  const canvas = document.createElement('canvas')
  canvas.width = SAMPLE_SIDE
  canvas.height = SAMPLE_SIDE
  const context = canvas.getContext('2d')!
  context.drawImage(frame.source, 0, 0, SAMPLE_SIDE, SAMPLE_SIDE)
  return context.getImageData(0, 0, SAMPLE_SIDE, SAMPLE_SIDE).data
}

/** The video has a picture to draw: enough data and a size (an iPhone rear camera has neither at first). */
export const videoReady = (video: Pick<HTMLVideoElement, 'readyState' | 'videoWidth' | 'videoHeight'>): boolean =>
  video.readyState >= 2 && video.videoWidth > 0 && video.videoHeight > 0

/** Waits for the first real frame; false if it does not come in `timeoutMs`. */
export async function waitForFirstFrame(
  video: Pick<HTMLVideoElement, 'readyState' | 'videoWidth' | 'videoHeight'>,
  timeoutMs = 5000,
  sleep: (ms: number) => Promise<void> = (ms) => new Promise((r) => setTimeout(r, ms)),
): Promise<boolean> {
  for (let waited = 0; waited <= timeoutMs; waited += 100) {
    if (videoReady(video)) return true
    await sleep(100)
  }
  return false
}

/** The current video frame copied to a canvas, so the camera can keep running or stop. */
export function grabFrame(video: HTMLVideoElement): Frame {
  const canvas = document.createElement('canvas')
  canvas.width = video.videoWidth
  canvas.height = video.videoHeight
  canvas.getContext('2d')!.drawImage(video, 0, 0)
  return { source: canvas, width: canvas.width, height: canvas.height }
}
