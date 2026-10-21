// Geolocation (spec 3.2 step 2-3, 6.1.5).

export const MAX_ACCURACY_M = 100

export interface Position {
  lat: number
  lon: number
  accuracy: number
}

/** Shelter creation and in-person endorsement need `accuracy <= 100 m` (spec 4.2). */
export const accuracyOk = (accuracy: number): boolean => accuracy <= MAX_ACCURACY_M

export function getPosition(geo: Pick<Geolocation, 'getCurrentPosition'> = navigator.geolocation): Promise<Position> {
  return new Promise((resolve, reject) =>
    geo.getCurrentPosition(
      (p) => resolve({ lat: p.coords.latitude, lon: p.coords.longitude, accuracy: p.coords.accuracy }),
      (e) => reject(e),
      { enableHighAccuracy: true, timeout: 30_000, maximumAge: 0 },
    ),
  )
}
