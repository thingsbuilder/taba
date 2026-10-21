import QRCode from 'qrcode'
import { expect, it } from 'vitest'
import { decodeQrPixels, qrSvg } from './qr'

/** Dark modules of the SVG: the stroke path is `M x y` then `h n` (draw n) and `m dx dy` (move). */
function darkModules(svg: string, margin: number): Set<string> {
  const d = /<path stroke="#000000" d="([^"]+)"/.exec(svg)![1]!
  const cells = new Set<string>()
  let x = 0
  let y = 0
  for (const [, cmd, a, b] of d.matchAll(/([Mhm])(-?[\d.]+)(?: (-?[\d.]+))?/g)) {
    if (cmd === 'M') [x, y] = [Number(a), Number(b)]
    else if (cmd === 'm') [x, y] = [x + Number(a), y + Number(b)]
    else {
      for (let i = 0; i < Number(a); i++) cells.add(`${x + i - margin},${Math.floor(y) - margin}`)
      x += Number(a)
    }
  }
  return cells
}

it('draws exactly the modules of the QR code of the URL', async () => {
  const url = 'https://taba.example.org/invite/AbC_-123'
  const { modules } = QRCode.create(url, {})
  const dark = darkModules(await qrSvg(url), 1)
  expect(dark.size).toBeGreaterThan(50)
  for (let y = 0; y < modules.size; y++) {
    for (let x = 0; x < modules.size; x++) expect(dark.has(`${x},${y}`), `${x},${y}`).toBe(modules.get(y, x) === 1)
  }
})

it('gives another picture for another text', async () => {
  expect(await qrSvg('https://taba.example.org/invite/a')).not.toBe(await qrSvg('https://taba.example.org/invite/b'))
})

it('jsQR (the reserve for Safari) reads back the card code from the QR pixels', () => {
  const { modules } = QRCode.create('ABCD1234', {})
  const scale = 6
  const quiet = 4
  const side = (modules.size + quiet * 2) * scale
  const pixels = new Uint8ClampedArray(side * side * 4).fill(255)
  for (let y = 0; y < side; y++) {
    for (let x = 0; x < side; x++) {
      const mx = Math.floor(x / scale) - quiet
      const my = Math.floor(y / scale) - quiet
      if (mx >= 0 && my >= 0 && mx < modules.size && my < modules.size && modules.get(my, mx) === 1) pixels.fill(0, (y * side + x) * 4, (y * side + x) * 4 + 3)
    }
  }
  expect(decodeQrPixels(pixels, side, side)).toBe('ABCD1234')
  expect(decodeQrPixels(new Uint8ClampedArray(100 * 100 * 4).fill(255), 100, 100)).toBeNull()
})
