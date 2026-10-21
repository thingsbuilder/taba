import type { Db } from './db'
import { type Language, saveLanguage, setLanguage } from './i18n'
import type { Queue } from './queue'
import { type LocalSession, updateProfile } from './sessions'

/**
 * The volunteer picks PT or EN (spec 11.1): the screen changes and the choice is saved on the phone
 * (it survives a reload). Signed in, the profile follows (`PATCH /v1/me` through the queue, so it works
 * offline), and the language of the profile is what any other phone shows after login.
 */
export async function chooseLanguage(db: Db, queue: Queue, active: LocalSession | null, language: Language): Promise<void> {
  setLanguage(language)
  await saveLanguage(db, language)
  if (!active) return
  await updateProfile(db, active.volunteer_id, { language })
  await queue.enqueue({ method: 'PATCH', path: '/v1/me', body: { language }, volunteer_id: active.volunteer_id })
}
