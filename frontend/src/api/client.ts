import createClient, { type Middleware } from 'openapi-fetch'
import { env } from '../config/env'
import { session } from '../state/session'
import type { paths } from './schema'
import type { ErrorCode } from './types'

export class ApiError extends Error {
  status: number
  code: ErrorCode | 'network' | 'unknown'
  details?: unknown
  constructor(status: number, code: ApiError['code'], message: string, details?: unknown) {
    super(message)
    this.status = status
    this.code = code
    this.details = details
  }
}

// Mock mode resolves this once MSW is intercepting; live mode never waits.
let ready: Promise<unknown> = Promise.resolve()
export const setReady = (p: Promise<unknown>) => {
  ready = p.catch(() => {})
}
export const whenReady = () => ready

// Typed client generated from contract/openapi.yaml. openapi-fetch encodes
// path params, so ids from deep links can't rewrite the API path.
// fetch is looked up per call: in the iOS and Android apps there is no service
// worker, so mock mode patches window.fetch after this module loads.
export const http = createClient<paths>({ baseUrl: env.apiBase, fetch: (req) => globalThis.fetch(req) })

const auth: Middleware = {
  async onRequest({ request, schemaPath }) {
    await ready
    const isPublic = schemaPath.startsWith('/v1/auth/') || schemaPath.startsWith('/v1/public/')
    if (!isPublic && session.token) request.headers.set('Authorization', `Bearer ${session.token}`)
    return request
  },
}
http.use(auth)

type Result<T> = { data?: T; error?: unknown; response: Response }

// Unwraps a call: returns data, or throws ApiError with the contract's error code.
export async function call<T>(p: Promise<Result<T>>): Promise<NonNullable<T>> {
  let r: Result<T>
  try {
    r = await p
  } catch {
    throw new ApiError(0, 'network', 'Can’t reach Ferry right now. Check your connection.')
  }
  if (!r.response.ok) {
    const err = (r.error as { error?: { code?: ErrorCode; message?: string; details?: unknown } } | undefined)?.error
    if (r.response.status === 401) session.set(null)
    throw new ApiError(r.response.status, err?.code ?? 'unknown', err?.message ?? `Request failed (${r.response.status})`, err?.details)
  }
  return r.data as NonNullable<T>
}
