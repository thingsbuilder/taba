import { expect, it } from 'vitest'
import { occupancyView } from './occupancy'

it('142 of 200 with 58 vacancies and 71 %', () => {
  expect(occupancyView({ occupancy: 142, capacity: 200, vacancies: 58 })).toEqual({ occupied: 142, capacity: 200, vacancies: 58, percent: 71, full: false })
})
it('is full with no vacancies and the bar stops at 100 %', () => {
  expect(occupancyView({ occupancy: 210, capacity: 200, vacancies: 0 })).toMatchObject({ percent: 100, full: true, vacancies: 0 })
})
it('without capacity there is no bar and no vacancy count', () => {
  expect(occupancyView({ occupancy: 12, capacity: null, vacancies: null })).toEqual({ occupied: 12, capacity: null, vacancies: null, percent: 0, full: false })
})
