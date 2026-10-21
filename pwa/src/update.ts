import { registerSW } from 'virtual:pwa-register'

const CHECK_INTERVAL_MS = 60 * 60_000

/**
 * Service worker update strategy: a new worker installs in the background and waits.
 * The app then shows "new version, tap to update"; only the tap activates it and reloads,
 * so a volunteer in the middle of a check-in is never reloaded under their feet.
 * The browser checks for a new worker on every navigation; we also check when the app
 * comes back to the foreground and every hour while open.
 */
export function watchForUpdates(onNeedRefresh: (apply: () => void) => void): void {
  const updateSW = registerSW({
    onNeedRefresh: () => onNeedRefresh(() => void updateSW(true)),
    onRegisteredSW(_url, registration) {
      if (!registration) return
      const check = () => void registration.update().catch(() => {})
      setInterval(check, CHECK_INTERVAL_MS)
      document.addEventListener('visibilitychange', () => document.visibilityState === 'visible' && check())
    },
  })
}
