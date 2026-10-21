import { registerSW } from 'virtual:pwa-register'
import { logEvent } from './diagnostics'

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
    onNeedRefresh: () => {
      logEvent('sw_update_available')
      onNeedRefresh(() => {
        logEvent('sw_update_apply')
        void updateSW(true)
      })
    },
    onRegisterError: (error) => logEvent('sw_register_error', { cause: (error as { name?: string } | null)?.name }),
    onRegisteredSW(_url, registration) {
      if (!registration) return
      logEvent('sw_registered')
      const check = () => void registration.update().catch(() => {})
      setInterval(check, CHECK_INTERVAL_MS)
      document.addEventListener('visibilitychange', () => document.visibilityState === 'visible' && check())
    },
  })
}
