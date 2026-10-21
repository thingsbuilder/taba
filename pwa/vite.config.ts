import preact from '@preact/preset-vite'
import type { Plugin } from 'vite'
import { VitePWA } from 'vite-plugin-pwa'
import { defineConfig } from 'vitest/config'
import pkg from './package.json' with { type: 'json' }

// design/tokens.css starts with an @import of Google Fonts. In production the CSP is
// default-src 'self' and the app must open offline, so the fonts are bundled
// (src/fonts.css) and the remote import is stripped here, without editing the generated file.
const stripRemoteFonts = (): Plugin => ({
  name: 'taba-strip-remote-fonts',
  enforce: 'pre',
  transform(code, id) {
    if (!id.endsWith('design/tokens.css')) return
    const out = code.replace(/@import url\("https:\/\/fonts\.googleapis\.com[^"]*"\);?/, '')
    if (out.includes('googleapis')) throw new Error('design/tokens.css still loads remote fonts')
    return out
  },
})

export default defineConfig({
  define: { __APP_VERSION__: JSON.stringify(pkg.version) },
  plugins: [
    stripRemoteFonts(),
    preact(),
    VitePWA({
      // The app shows "new version, tap to update" (src/update.ts); nothing swaps silently.
      registerType: 'prompt',
      injectRegister: false,
      includeAssets: ['favicon.svg', 'icons/apple-touch-icon.png'],
      manifest: {
        name: 'Taba',
        short_name: 'Taba',
        description: 'Rede de abrigos para desastres',
        lang: 'pt-BR',
        start_url: '/',
        scope: '/',
        display: 'standalone',
        background_color: '#FBF8F3',
        theme_color: '#0F5C5A',
        icons: [
          { src: '/icons/icon-192.png', sizes: '192x192', type: 'image/png', purpose: 'any' },
          { src: '/icons/icon-512.png', sizes: '512x512', type: 'image/png', purpose: 'any' },
          { src: '/icons/icon-maskable-512.png', sizes: '512x512', type: 'image/png', purpose: 'maskable' },
        ],
      },
      workbox: {
        globPatterns: ['**/*.{js,css,html,woff2,svg,png,webmanifest}'],
        navigateFallback: '/index.html',
        // The API is never an app-shell route: /v1/* always goes to the network.
        navigateFallbackDenylist: [/^\/v1\//],
        cleanupOutdatedCaches: true,
      },
    }),
  ],
  server: { fs: { allow: ['..'] }, proxy: { '/v1': 'http://127.0.0.1:8080' } },
  test: { include: ['src/**/*.test.ts'], setupFiles: ['fake-indexeddb/auto'] },
})
