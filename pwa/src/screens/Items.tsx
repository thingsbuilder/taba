import { useEffect, useState } from 'preact/hooks'
import type { AppContext } from '../app-context'
import { errorKey } from '../app-context'
import { useOnline } from '../hooks'
import { t } from '../i18n'
import { type ItemRow, MAX_NAME_CHARS, MAX_PERIOD_DAYS, canAddRow, itemsErrorKey, itemsFromRows, newRow, rowsFromItems, validateRows } from '../items'
import { navigate } from '../nav'
import { roleIn } from '../sessions'
import { BackLink } from './BackLink'

/** `/shelter/:id/items` (spec 3.5.2): the admin sets what the shelter gives. Saving needs signal. */
export function Items({ ctx, shelterId }: { ctx: AppContext; shelterId: string }) {
  const { active, api } = ctx
  const online = useOnline()
  const [rows, setRows] = useState<ItemRow[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [saved, setSaved] = useState(false)
  const isAdmin = !!active && roleIn(active, shelterId) === 'admin'

  useEffect(() => {
    if (!active) return navigate('/login', true)
    if (!isAdmin) return navigate(`/shelter/${shelterId}`, true)
    if (online && rows === null) {
      void api.getShelter(active.secret, shelterId).then((res) => {
        if (res.ok) setRows(rowsFromItems(res.data.shelter.items ?? {}))
        else setError(t(errorKey(res.error)))
      })
    }
  }, [online, shelterId])
  if (!active || !isAdmin) return null
  const secret = active.secret

  const edit = (i: number, patch: Partial<ItemRow>) => {
    setSaved(false)
    setRows((rows ?? []).map((r, j) => (j === i ? { ...r, ...patch } : r)))
  }

  async function save() {
    const problem = validateRows(rows ?? [])
    if (problem) return setError(t(problem))
    setError(null)
    const res = await api.putItems(secret, shelterId, { items: itemsFromRows(rows ?? []) })
    if (!res.ok) return setError(t(itemsErrorKey(res.status) ?? errorKey(res.error)))
    setRows(rowsFromItems(res.data.items))
    setSaved(true)
  }

  return (
    <main class="screen">
      <BackLink />
      <h1 class="title">{t('items.title')}</h1>
      {!online && <p class="notice notice-warning" role="status">{t('items.needs_signal')}</p>}
      {online && rows === null && !error && <p class="body" role="status">{t('items.loading')}</p>}
      {rows?.length === 0 && <p class="body">{t('items.empty')}</p>}
      {rows?.map((row, i) => (
        <section class="card" key={i}>
          <label class="field">
            {t('items.name')}
            <input type="text" maxLength={MAX_NAME_CHARS} value={row.name} onInput={(e) => edit(i, { name: e.currentTarget.value })} />
          </label>
          <label class="field">
            {t('items.scope')}
            <select value={row.scope} onChange={(e) => edit(i, { scope: e.currentTarget.value as ItemRow['scope'] })}>
              <option value="per_person">{t('items.scope_person')}</option>
              <option value="per_household">{t('items.scope_household')}</option>
            </select>
          </label>
          <label class="check">
            <input type="checkbox" checked={row.oncePerEvent} onChange={(e) => edit(i, { oncePerEvent: e.currentTarget.checked })} />
            {t('items.once_per_event')}
          </label>
          {!row.oncePerEvent && (
            <label class="field">
              {t('items.period_days')}
              <input type="number" min={1} max={MAX_PERIOD_DAYS} inputMode="numeric" value={row.periodDays} onInput={(e) => edit(i, { periodDays: e.currentTarget.value })} />
            </label>
          )}
          <label class="check">
            <input type="checkbox" checked={row.highValue} onChange={(e) => edit(i, { highValue: e.currentTarget.checked })} />
            {t('items.high_value')}
          </label>
          <button type="button" class="button button-secondary" onClick={() => { setSaved(false); setRows((rows ?? []).filter((_, j) => j !== i)) }}>{t('items.remove')}</button>
        </section>
      ))}
      {rows && <button type="button" class="button button-secondary" disabled={!canAddRow(rows)} onClick={() => setRows([...rows, newRow()])}>{t('items.add')}</button>}
      {rows && !canAddRow(rows) && <p class="caption" role="status">{t('items.error_too_many')}</p>}
      {error && <p class="notice notice-error" role="alert">{error}</p>}
      {saved && <p class="notice" role="status">{t('items.saved')}</p>}
      {rows && <button type="button" class="button" disabled={!online} onClick={() => void save()}>{t('shelter.save')}</button>}
    </main>
  )
}
