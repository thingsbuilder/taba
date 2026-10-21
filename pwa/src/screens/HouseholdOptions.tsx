import { useState } from 'preact/hooks'
import type { AppContext } from '../app-context'
import { createSoloHousehold } from '../households'
import { t } from '../i18n'
import { navigate } from '../nav'

/** What to do about the household of a person who has none: a new one, join one, or alone. Nothing is created until one is chosen. */
export function HouseholdOptions({ ctx, shelterId, personId }: { ctx: AppContext; shelterId: string; personId: string }) {
  const { active, db, queue } = ctx
  const [busy, setBusy] = useState(false)

  async function alone() {
    if (!active) return
    setBusy(true)
    const id = await createSoloHousehold(db, queue, { volunteer_id: active.volunteer_id, shelter_id: shelterId }, personId)
    navigate(`/shelter/${shelterId}/households/${id}`)
  }

  return (
    <>
      <h2 class="subtitle">{t('checkin.household_question')}</h2>
      <button type="button" class="button button-secondary" onClick={() => navigate(`/shelter/${shelterId}/households/new?person=${personId}`)}>{t('checkin.household_new')}</button>
      <button type="button" class="button button-secondary" onClick={() => navigate(`/shelter/${shelterId}/households/new`)}>{t('checkin.household_join')}</button>
      <button type="button" class="button button-secondary" disabled={busy} onClick={() => void alone()}>{t('checkin.household_alone')}</button>
    </>
  )
}
