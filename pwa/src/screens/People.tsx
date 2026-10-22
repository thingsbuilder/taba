import { useEffect, useState } from 'preact/hooks'
import type { PersonSummary } from '../api-phase3'
import type { AppContext } from '../app-context'
import type { QueueItem } from '../db'
import { useOnline } from '../hooks'
import { getLanguage, t } from '../i18n'
import { navigate } from '../nav'
import { type DisplayName, cachePeople, cachedPeople, displayName, initials, matchesQuery, pendingPeople, relativeTime } from '../people'
import { BackLink } from './BackLink'

export const nameText = (n: DisplayName): string =>
  n.kind === 'name' ? n.text : n.kind === 'wristband' ? t('person.wristband', { code: n.text }) : t('person.no_name')

/** One person of the list (design: avatar, name, time, level chip). Without `onOpen` the row is not a button (a pending one). */
export function PersonRow({ name, time, level, isChild, pending, onOpen }: {
  name: DisplayName
  time?: string
  level?: PersonSummary['level']
  isChild?: boolean
  pending?: boolean
  onOpen?: () => void
}) {
  const inner = (
    <>
      <span class="avatar" aria-hidden="true">{initials(name)}</span>
      <span class="row-text">
        <span class={name.kind === 'wristband' ? 'body-strong code' : 'body-strong'}>{nameText(name)}</span>
        {time && <span class="caption">{time}</span>}
      </span>
      {isChild && <span class="chip chip-inline chip-neutral">{t('person.child')}</span>}
      {pending && <span class="chip chip-inline chip-warning-inline">{t('person.pending')}</span>}
      {level && <span class={`chip chip-inline level-${level}`}>{t(`person.level.${level}`)}</span>}
    </>
  )
  return onOpen ? (
    <button type="button" class="card row" onClick={onOpen}>{inner}</button>
  ) : (
    <div class="card row">{inner}</div>
  )
}

type Tab = 'all' | 'needs_selfie'

/** `/shelter/:id/people`: everyone in the shelter and who still waits for a selfie (spec 3.4). */
export function People({ ctx, shelterId }: { ctx: AppContext; shelterId: string }) {
  const { active, api, db, queue } = ctx
  const online = useOnline()
  const [tab, setTab] = useState<Tab>('all')
  const [query, setQuery] = useState('')
  const [people, setPeople] = useState<PersonSummary[] | null>(null)
  const [needsSelfie, setNeedsSelfie] = useState<PersonSummary[]>([])
  const [hasMore, setHasMore] = useState({ all: false, needs_selfie: false })
  const [items, setItems] = useState<QueueItem[]>([])
  const [fromCache, setFromCache] = useState(false)

  useEffect(() => {
    if (!active) return navigate('/login', true)
    const load = () => void queue.list().then(setItems)
    load()
    return queue.subscribe(load)
  }, [])

  useEffect(() => {
    if (!active) return
    let alive = true
    if (!online) {
      void cachedPeople(db, shelterId).then((rows) => {
        if (!alive) return
        setPeople(rows)
        setFromCache(true)
      })
      return
    }
    void api.listPeople(active.secret, shelterId).then(async (res) => {
      if (!alive) return
      if (res.ok) {
        setPeople(res.data.people)
        setHasMore((cur) => ({ ...cur, all: res.data.has_more === true }))
        setFromCache(false)
        await cachePeople(db, shelterId, res.data.people, res.data.has_more === true)
      } else {
        setPeople(await cachedPeople(db, shelterId))
        setFromCache(true)
      }
    })
    void api.listNeedsSelfie(active.secret, shelterId).then((res) => {
      if (!alive || !res.ok) return
      setNeedsSelfie(res.data.people)
      setHasMore((cur) => ({ ...cur, needs_selfie: res.data.has_more === true }))
    })
    return () => {
      alive = false
    }
  }, [online, shelterId, items.length])

  if (!active) return null
  const lang = getLanguage()
  const now = Date.now()
  const shown = (tab === 'all' ? people ?? [] : needsSelfie).filter((p) => matchesQuery(p, query))
  const pending = tab === 'all' ? pendingPeople(items, shelterId).filter((p) => matchesQuery(p, query)) : []
  const empty = shown.length === 0 && pending.length === 0

  return (
    <>
      <div class="screen back-bar"><BackLink /></div>
      <main class="screen">
        <h1 class="title">{t('person.list_title')}</h1>
        <div class="lang" role="group">
          {(['all', 'needs_selfie'] as const).map((id) => (
            <button type="button" class="lang-option tab-option" aria-pressed={tab === id} onClick={() => setTab(id)}>
              {t(id === 'all' ? 'person.tab_all' : 'person.tab_needs_selfie')}
            </button>
          ))}
        </div>
        <label class="field">
          <span class="caption">{t('person.search')}</span>
          <input type="search" value={query} onInput={(e) => setQuery(e.currentTarget.value)} />
        </label>
        {fromCache && <p class="notice" role="status">{people && people.length > 0 ? t('person.cached') : t('person.needs_signal_list')}</p>}
        {!fromCache && hasMore[tab] && <p class="notice notice-warning" role="status">{t('person.has_more')}</p>}
        <ul class="list">
          {pending.map((p) => (
            <li key={p.queue_id}>
              <PersonRow name={displayName(p)} isChild={p.is_child} pending />
            </li>
          ))}
          {shown.map((p) => (
            <li key={p.id}>
              <PersonRow
                name={displayName(p)}
                time={p.updated_at ? relativeTime(p.updated_at, now, lang) : undefined}
                level={p.level}
                isChild={p.is_child}
                onOpen={() => navigate(`/person/${p.id}`)}
              />
            </li>
          ))}
        </ul>
        {empty && people !== null && (
          <p class="body">{query ? t('person.empty_search') : t(tab === 'all' ? 'person.empty' : 'person.empty_needs_selfie')}</p>
        )}
      </main>
    </>
  )
}
