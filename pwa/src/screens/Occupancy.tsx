import type { ShelterView } from '../api'
import { t } from '../i18n'
import { occupancyView } from '../occupancy'

/** The occupancy card of the design: "142 of 200", a bar and the vacancies. */
export function Occupancy({ shelter }: { shelter: Pick<ShelterView, 'occupancy' | 'capacity' | 'vacancies'> }) {
  const v = occupancyView(shelter)
  return (
    <section class="card">
      <h2 class="subtitle">{t('checkin.occupancy_title')}</h2>
      <p class="body-strong">
        {v.capacity === null ? t('checkin.occupancy_no_capacity', { n: v.occupied }) : t('checkin.occupancy', { n: v.occupied, capacity: v.capacity })}
      </p>
      {v.capacity !== null && (
        <>
          <div class="occupancy-bar" role="progressbar" aria-label={t('checkin.occupancy_bar')} aria-valuemin={0} aria-valuemax={v.capacity} aria-valuenow={Math.min(v.occupied, v.capacity)}>
            <div class="occupancy-fill" style={{ width: `${v.percent}%` }} />
          </div>
          <p class="caption">{v.full ? t('checkin.full') : t('checkin.vacancies', { n: v.vacancies ?? 0 })}</p>
        </>
      )}
    </section>
  )
}
