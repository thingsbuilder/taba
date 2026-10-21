// The occupancy card of the shelter screen: "142 of 200" with the vacancies and a bar (design/README.md, spec 4.2).

export interface OccupancyView {
  occupied: number
  capacity: number | null
  vacancies: number | null
  /** 0 to 100; 0 when the shelter has no capacity set. */
  percent: number
  full: boolean
}

export function occupancyView(s: { occupancy: number; capacity: number | null; vacancies: number | null }): OccupancyView {
  const capacity = s.capacity && s.capacity > 0 ? s.capacity : null
  const vacancies = capacity === null ? null : (s.vacancies ?? Math.max(0, capacity - s.occupancy))
  return {
    occupied: s.occupancy,
    capacity,
    vacancies,
    percent: capacity === null ? 0 : Math.min(100, Math.round((s.occupancy / capacity) * 100)),
    full: vacancies === 0,
  }
}
