import jsQR from 'jsqr'
import QRCode from 'qrcode'

/** SVG markup of a QR code (spec 6.1). The text is a URL the server built, never user input. */
export const qrSvg = (text: string): Promise<string> => QRCode.toString(text, { type: 'svg', margin: 1 })

/** `jsQR` on RGBA pixels: the reserve for browsers without `BarcodeDetector` (Safari on iOS). */
export const decodeQrPixels = (data: Uint8ClampedArray, width: number, height: number): string | null =>
  jsQR(data, width, height)?.data || null

interface Detector {
  detect(source: HTMLVideoElement): Promise<{ rawValue: string }[]>
}
declare const BarcodeDetector: (new (opts: { formats: string[] }) => Detector) | undefined

let detector: Detector | null | undefined

/** One QR read from a playing video: `BarcodeDetector` when the browser has it, `jsQR` otherwise (spec 6.1). */
export async function readQr(video: HTMLVideoElement, canvas: HTMLCanvasElement): Promise<string | null> {
  if (detector === undefined) detector = typeof BarcodeDetector === 'function' ? new BarcodeDetector({ formats: ['qr_code'] }) : null
  if (detector) {
    try {
      return (await detector.detect(video))[0]?.rawValue ?? null
    } catch {
      // The detector exists but failed on this frame: try the reserve.
    }
  }
  const { videoWidth: w, videoHeight: h } = video
  if (!w || !h) return null
  canvas.width = w
  canvas.height = h
  const context = canvas.getContext('2d', { willReadFrequently: true })
  if (!context) return null
  context.drawImage(video, 0, 0, w, h)
  return decodeQrPixels(context.getImageData(0, 0, w, h).data, w, h)
}
