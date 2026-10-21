export interface ApiStatus {
  unico: 'ok' | 'degraded'
  reason?: 'limit' | 'unavailable'
}

/** `GET /v1/status` (spec 8.1). Returns null when the server cannot be reached. */
export async function getStatus(fetchFn: typeof fetch = fetch): Promise<ApiStatus | null> {
  try {
    const res = await fetchFn('/v1/status', { cache: 'no-store' })
    return res.ok ? ((await res.json()) as ApiStatus) : null
  } catch {
    return null
  }
}
