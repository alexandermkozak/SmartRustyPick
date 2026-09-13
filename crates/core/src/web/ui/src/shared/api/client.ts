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
    // Loose equality on purpose: an unpopulated `record` is absent or null.
    if (response.record == null) throw new ApiError(502, 'The database returned no data')
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

/**
 * A request whose reply is bytes rather than JSON, with the filename the server
 * chose.
 *
 * The one endpoint shaped like this is the archive download. It is here rather
 * than in the slice because this module owns the transport - what the bytes
 * mean is still the feature's business.
 */
export async function fetchBytes(path: string): Promise<{blob: Blob; filename: string}> {
    const response = await send(path, {})
    // A refusal is still JSON, so it is read and raised the way every other
    // failure is rather than handed back as a zero-byte download.
    if (!response.ok) throw await failure(response)
    return {blob: await response.blob(), filename: filenameFrom(response)}
}

/**
 * A request whose body is bytes rather than JSON, answering with the usual
 * envelope. The archive upload.
 */
export async function sendBytes<T>(path: string, body: Blob): Promise<T> {
    const response = await send(path, {
        method: 'POST',
        body,
        headers: {'Content-Type': 'application/octet-stream'},
    })
    if (!response.ok) throw await failure(response)
    return (await response.json()) as T
}

/** The fetch every call here goes through, with the dev-token rule applied once. */
async function send(path: string, init: RequestInit): Promise<Response> {
    const headers = new Headers(init.headers)
    const token = developmentToken()
    if (token) headers.set('Authorization', `Bearer ${token}`)
    try {
        return await fetch(path, {credentials: 'same-origin', ...init, headers})
    } catch (cause) {
        throw new ApiError(
            0,
            cause instanceof Error ? cause.message : 'The dashboard is unreachable',
        )
    }
}

/** A non-OK response as an `ApiError`, taking the server's own message if it sent one. */
async function failure(response: Response): Promise<ApiError> {
    let message = `Request failed (${response.status})`
    try {
        const payload = (await response.json()) as {error?: string} | null
        if (payload?.error) message = payload.error
    } catch {
        // Not JSON. The status is all there is, and it says enough.
    }
    return new ApiError(response.status, message)
}

/** The name from `Content-Disposition`, or a fallback the browser can still save. */
function filenameFrom(response: Response): string {
    const header = response.headers.get('Content-Disposition') ?? ''
    const match = /filename="?([^"]+)"?/.exec(header)
    return match?.[1] ?? 'archive.srp'
}

/**
 * Hands the viewer bytes the page is holding.
 *
 * The object URL is revoked rather than left behind: an archive is a database,
 * and a page that keeps one alive in the tab for as long as it is open is
 * holding a copy of the data nobody asked it to keep.
 */
export function saveBlob(filename: string, blob: Blob): void {
    const url = URL.createObjectURL(blob)
    const link = document.createElement('a')
    link.href = url
    link.download = filename
    document.body.appendChild(link)
    link.click()
    link.remove()
    window.setTimeout(() => URL.revokeObjectURL(url), 10_000)
}

/** Percent-encodes one path segment; account and file names are user data. */
export const encode = encodeURIComponent
