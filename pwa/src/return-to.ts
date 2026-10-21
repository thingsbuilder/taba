import type { Db } from './db'

// A page that needs a session sends the visitor to /login and brings them back afterwards
// (the journey returns in a fresh page load, so the path waits in IndexedDB).

/** Only in-app paths: an absolute or protocol-relative URL would be an open redirect. */
const safe = (path: unknown): path is string => typeof path === 'string' && path.startsWith('/') && !path.startsWith('//')

export const setReturnTo = (db: Db, path: string) => db.put('config', path, 'return_to')

export async function takeReturnTo(db: Db): Promise<string | null> {
  const path = await db.get('config', 'return_to')
  await db.delete('config', 'return_to')
  return safe(path) ? path : null
}
