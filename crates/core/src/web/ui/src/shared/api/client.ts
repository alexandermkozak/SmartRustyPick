/**
 * The transport every feature's `api.ts` is built on.
 *
 * This layer knows about HTTP and about the protocol envelope, and nothing
 * about what any particular endpoint means. Features own their own calls, so a
 * new one is added inside its slice rather than in a growing shared file.
 */

import type {ProtocolResponse} from './protocol'

export class ApiError extends Error {
    readonly status: number

    constructor(status: number, message: string) {
        super(message)
        this.name = 'ApiError'
        this.status = status
    }

    /** The session is no longer authenticated, so retrying will not help. */
    get unauthorized(): boolean {
        return this.status === 401
    }
}

/**
 * In production the token lives in an `HttpOnly` cookie the server set, and the
 * page deliberately cannot read it: same-origin requests carry it on their own.
 *
 * `npm run dev` serves the page from Vite instead, so no such cookie exists for
 * that origin. Only then does the token get taken from `?token=` and replayed
 * as a bearer header. Gating this on `import.meta.env.DEV` means the production
 * bundle has no path that puts the token anywhere script-readable.
 */
const developmentToken = (): string | null => {
    if (!import.meta.env.DEV) return null
    const fromUrl = new URLSearchParams(window.location.search).get('token')
    if (fromUrl) sessionStorage.setItem('srp_dev_token', fromUrl)
    return sessionStorage.getItem('srp_dev_token')
}

/** One request, with failures normalised into `ApiError`. */
export async function call<T>(path: string, init: RequestInit = {}): Promise<T> {
    const headers = new Headers(init.headers)
    if (init.body) headers.set('Content-Type', 'application/json')
    const token = developmentToken()
    if (token) headers.set('Authorization', `Bearer ${token}`)

    let response: Response
    try {
        response = await fetch(path, {credentials: 'same-origin', ...init, headers})
    } catch (cause) {
        // A network-level failure: the server went away, or the page is offline.
        throw new ApiError(
            0,
            cause instanceof Error ? cause.message : 'The dashboard is unreachable',
        )
    }

    let payload: unknown = null
    try {
        payload = await response.json()
    } catch {
        // A body that is not JSON is a bug on our side; the status still says enough.
    }

    if (!response.ok) {
        const message =
            (payload as {error?: string} | null)?.error ?? `Request failed (${response.status})`
        throw new ApiError(response.status, message)
    }
    return payload as T
}

/** Unwraps the envelope's `record`, which every single-object command fills. */
export async function record<T>(path: string, init?: RequestInit): Promise<T> {
    const response = await call<ProtocolResponse<T>>(path, init)
    if (response.record === null) throw new ApiError(502, 'The database returned no data')
    return response.record
}

/** Unwraps `results`, which the list commands fill with `[name, details]` pairs. */
export async function pairs<T>(path: string): Promise<Array<[string, T]>> {
    const response = await call<ProtocolResponse<never, [string, T]>>(path)
    return response.results ?? []
}

/** Unwraps `keys`, the plain list of names `LIST.FILES` returns. */
export async function keys(path: string): Promise<string[]> {
    const response = await call<ProtocolResponse>(path)
    return response.keys ?? []
}

/** Percent-encodes one path segment; account and file names are user data. */
export const encode = encodeURIComponent
