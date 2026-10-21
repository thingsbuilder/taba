import { expect, it } from 'vitest'
import { qrSvg } from './qr'

it('renders an svg for a URL', async () => {
  const svg = await qrSvg('https://taba.example.org/invite/abc')
  expect(svg.startsWith('<svg')).toBe(true)
  expect(svg).toContain('</svg>')
})
