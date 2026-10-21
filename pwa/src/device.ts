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

/** Spec 3.0.2: on iOS, `navigator.standalone !== true` blocks the app until it is installed. */
export function isIosOutsideStandalone(nav: IosProbe): boolean {
  const iphoneOrIpod = /iPhone|iPod/.test(nav.userAgent)
  // iPadOS 13+ reports itself as a Mac; touch support tells them apart.
  const ipad = /iPad/.test(nav.userAgent) || (nav.platform === 'MacIntel' && nav.maxTouchPoints > 1)
  return (iphoneOrIpod || ipad) && nav.standalone !== true
}

/** Spec 3.0.3: `false` (or no API) means the browser may erase our data; the UI warns permanently. */
export async function requestPersistence(storage: Pick<StorageManager, 'persist'> | undefined): Promise<boolean> {
  try {
    return (await storage?.persist?.()) === true
  } catch {
    return false
  }
}
