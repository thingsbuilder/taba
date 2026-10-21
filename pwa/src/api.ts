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

export type ApiResult<T> = { ok: true; data: T } | { ok: false; status: number; error: string }

export interface VolunteerView {
  id: string
  name: string
  phone: string | null
  show_phone: boolean
  language: 'pt-BR' | 'en'
}

export interface SessionResponse {
  session: string
  expires_at: string
  volunteer: VolunteerView
}

export interface Me {
  volunteer: VolunteerView
  global_roles: string[]
  memberships: unknown[]
}

export interface ProfileInput {
  name: string
  phone?: string
  show_phone: boolean
  language: 'pt-BR' | 'en'
}

/** The calls of spec 8.1. A network failure is `{ ok: false, status: 0, error: 'network' }`. */
export function createApi(fetchFn: typeof fetch, deviceId: () => Promise<string>) {
  async function call<T>(method: string, path: string, opts: { secret?: string; body?: unknown } = {}): Promise<ApiResult<T>> {
    const headers: Record<string, string> = { 'X-Device-Id': await deviceId() }
    if (opts.body !== undefined) headers['Content-Type'] = 'application/json'
    if (opts.secret) headers.Authorization = `Bearer ${opts.secret}`
    let res: Response
    try {
      res = await fetchFn(path, { method, headers, body: opts.body === undefined ? undefined : JSON.stringify(opts.body) })
    } catch {
      return { ok: false, status: 0, error: 'network' }
    }
    const body = res.status === 204 ? null : await res.json().catch(() => null)
    // 202 is "capture_pending": a 2xx that is not a result yet.
    if (res.ok && res.status !== 202) return { ok: true, data: body as T }
    return { ok: false, status: res.status, error: (body as { error?: string } | null)?.error ?? `http_${res.status}` }
  }

  return {
    createCapture: () => call<{ capture_id: string; link: string; expires_at: string }>('POST', '/v1/captures', { body: { purpose: 'login' } }),
    createSession: (captureId: string, profile?: ProfileInput) =>
      call<SessionResponse>('POST', '/v1/sessions', { body: { capture_id: captureId, ...(profile ? { profile } : {}) } }),
    getMe: (secret: string) => call<Me>('GET', '/v1/me', { secret }),
    deleteSession: (secret: string) => call<null>('DELETE', '/v1/sessions/current', { secret }),
  }
}

export type Api = ReturnType<typeof createApi>
