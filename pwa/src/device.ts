import type { Db } from './db'

/** Spec 3.0.1: first access generates a UUID v4 `device_id` and keeps it in IndexedDB. */
export async function getOrCreateDeviceId(db: Db): Promise<string> {
  const saved = await db.get('config', 'device_id')
  if (typeof saved === 'string') return saved
  const id = crypto.randomUUID()
  await db.put('config', id, 'device_id')
  return id
}

export interface IosProbe {
  userAgent: string
  platform: string
  maxTouchPoints: number
  standalone?: boolean
}

/** Spec 3.0.2: on iOS, `navigator.standalone !== true` means a browser tab, where the browser may erase our data. */
export function isIosOutsideStandalone(nav: IosProbe): boolean {
  const iphoneOrIpod = /iPhone|iPod/.test(nav.userAgent)
  // iPadOS 13+ reports itself as a Mac; touch support tells them apart.
  const ipad = /iPad/.test(nav.userAgent) || (nav.platform === 'MacIntel' && nav.maxTouchPoints > 1)
  return (iphoneOrIpod || ipad) && nav.standalone !== true
}

/** Spec 3.0.3: asks the browser to keep our data; `false` (or no API) means it may erase it. */
export async function requestPersistence(storage: Pick<StorageManager, 'persist'> | undefined): Promise<boolean> {
  try {
    return (await storage?.persist?.()) === true
  } catch {
    return false
  }
}

/** How long "not now" on the install invitation hides it (spec 3.0.2). */
export const INSTALL_PROMPT_SNOOZE_MS = 7 * 24 * 60 * 60 * 1000
