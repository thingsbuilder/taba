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

export function openCamera(facing: Facing, nav: { mediaDevices?: Pick<MediaDevices, 'getUserMedia'> } = navigator): Promise<MediaStream> {
  if (!cameraSupported(nav)) return Promise.reject(new DOMException('no camera', 'NotSupportedError'))
  return nav.mediaDevices!.getUserMedia({ video: { facingMode: facing }, audio: false })
}

export const stopStream = (stream: MediaStream | null): void => stream?.getTracks().forEach((t) => t.stop())

/** The current video frame copied to a canvas, so the camera can keep running or stop. */
export function grabFrame(video: HTMLVideoElement): Frame {
  const canvas = document.createElement('canvas')
  canvas.width = video.videoWidth
  canvas.height = video.videoHeight
  canvas.getContext('2d')!.drawImage(video, 0, 0)
  return { source: canvas, width: canvas.width, height: canvas.height }
}
