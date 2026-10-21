// Image reduction (spec 6.1.4): the photo for the token and the 128x128 thumbnail, both JPEG from a canvas.

export const MAX_SIDE = 1280
/** The API refuses more than this with `413 image_too_large` (spec 5.3). */
export const MAX_BYTES = 800 * 1024
export const START_QUALITY = 0.9
export const QUALITY_STEP = 0.05
/** Below this the picture is useless to the face check; a photo still too big here is refused. */
export const MIN_QUALITY = 0.1
export const THUMBNAIL_SIDE = 128
export const THUMBNAIL_QUALITY = 0.6

/** Largest side at most `max`, same proportion; never enlarges. */
export function fitSize(width: number, height: number, max = MAX_SIDE): { width: number; height: number } {
  const scale = Math.min(1, max / Math.max(width, height))
  return { width: Math.round(width * scale), height: Math.round(height * scale) }
}

/** Centered square of the source, for the thumbnail. */
export function squareCrop(width: number, height: number): { sx: number; sy: number; side: number } {
  const side = Math.min(width, height)
  return { sx: Math.floor((width - side) / 2), sy: Math.floor((height - side) / 2), side }
}

/** Quality 0.9, dropping 0.05 until the file is below `maxBytes` (spec 6.1.4). Null if even the floor is too big. */
export async function encodeUnderLimit(encode: (quality: number) => Promise<Blob>, maxBytes = MAX_BYTES): Promise<Blob | null> {
  for (let q = START_QUALITY; q >= MIN_QUALITY - 1e-9; q = Math.round((q - QUALITY_STEP) * 100) / 100) {
    const blob = await encode(q)
    if (blob.size < maxBytes) return blob
  }
  return null
}

function toBlob(canvas: HTMLCanvasElement, quality: number): Promise<Blob> {
  return new Promise((resolve, reject) => canvas.toBlob((b) => (b ? resolve(b) : reject(new Error('encode_failed'))), 'image/jpeg', quality))
}

export interface Frame {
  source: CanvasImageSource
  width: number
  height: number
}

/** The photo that goes to the token: largest side 1280 px, JPEG under 800 KB. Null when it cannot get that small. */
export function photoForToken({ source, width, height }: Frame): Promise<Blob | null> {
  const size = fitSize(width, height)
  const canvas = document.createElement('canvas')
  canvas.width = size.width
  canvas.height = size.height
  canvas.getContext('2d')!.drawImage(source, 0, 0, size.width, size.height)
  return encodeUnderLimit((q) => toBlob(canvas, q))
}

/** 128x128 JPEG at 0.6, for a person without name or wristband (spec 4.6). */
export function thumbnailOf({ source, width, height }: Frame): Promise<Blob> {
  const { sx, sy, side } = squareCrop(width, height)
  const canvas = document.createElement('canvas')
  canvas.width = THUMBNAIL_SIDE
  canvas.height = THUMBNAIL_SIDE
  canvas.getContext('2d')!.drawImage(source, sx, sy, side, side, 0, 0, THUMBNAIL_SIDE, THUMBNAIL_SIDE)
  return toBlob(canvas, THUMBNAIL_QUALITY)
}

/** Standard base64 of the bytes, without a `data:` prefix (the API field is raw base64). */
export async function toBase64(blob: Blob): Promise<string> {
  const bytes = new Uint8Array(await blob.arrayBuffer())
  let binary = ''
  for (let i = 0; i < bytes.length; i += 0x8000) binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000))
  return btoa(binary)
}
