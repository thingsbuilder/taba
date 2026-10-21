import QRCode from 'qrcode'
import { expect, it } from 'vitest'
import { qrSvg } from './qr'

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
