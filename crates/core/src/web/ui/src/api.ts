/**
 * The dashboard's HTTP client.
 *
 * Every call is one endpoint, which is in turn one remote-protocol command.
 * Failures arrive as `{ "error": "..." }` with a status code, so they are
 * turned into a single `ApiError` the views can render without inspecting
 * status codes themselves.
 */

import type {
    AccountStats,
    ClientEntry,
    ClientInfo,
    FileStats,
    GeneratedCert,
    ProtocolResponse,
    ServerSnapshot,
} from './types'

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

async function call<T>(path: string, init: RequestInit = {}): Promise<T> {
    const headers = new Headers(init.headers)
    if (init.body) headers.set('Content-Type', 'application/json')
    const token = developmentToken()
    if (token) headers.set('Authorization', `Bearer ${token}`)

    let response: Response
    try {
        response = await fetch(path, {credentials: 'same-origin', ...init, headers})
    } catch (cause) {
        // A network-level failure: the server went away, or the page is offline.
        throw new ApiError(0, cause instanceof Error ? cause.message : 'The dashboard is unreachable')
    }

    let payload: unknown = null
    try {
        payload = await response.json()
    } catch {
        // A body that is not JSON is a bug on our side; the status still says enough.
    }

    if (!response.ok) {
        const message =
            (payload as { error?: string } | null)?.error ?? `Request failed (${response.status})`
        throw new ApiError(response.status, message)
    }
    return payload as T
}

/** Unwraps the protocol envelope's `record`, which every single-object command fills. */
async function record<T>(path: string, init?: RequestInit): Promise<T> {
    const response = await call<ProtocolResponse<T>>(path, init)
    if (response.record === null) throw new ApiError(502, 'The database returned no data')
    return response.record
}

/** Unwraps `results`, which the list commands fill with `[name, details]` pairs. */
async function pairs<T>(path: string): Promise<Array<[string, T]>> {
    const response = await call<ProtocolResponse<never, [string, T]>>(path)
    return response.results ?? []
}

const encode = encodeURIComponent

export const api = {
    /** Everything the overview needs: uptime, totals and open sessions. */
    stats: (): Promise<ServerSnapshot> => record<ServerSnapshot>('/api/stats'),

    async clients(): Promise<ClientEntry[]> {
        const results = await pairs<ClientInfo>('/api/clients')
        return results.map(([name, info]) => ({name, info}))
    },

    authorize: (client: {
        name: string
        thumbprint: string
        accounts: string[]
        is_admin: boolean
    }): Promise<unknown> => call('/api/clients', {method: 'POST', body: JSON.stringify(client)}),

    revoke: (name: string): Promise<unknown> =>
        call(`/api/clients/${encode(name)}`, {method: 'DELETE'}),

    changeAccounts: (name: string, accounts: string[], remove: boolean): Promise<unknown> =>
        call(`/api/clients/${encode(name)}/accounts`, {
            method: 'POST',
            body: JSON.stringify({accounts, remove}),
        }),

    generateCertificate: (request: {
        common_name: string
        accounts: string[]
        is_admin: boolean
    }): Promise<GeneratedCert> =>
        record<GeneratedCert>('/api/certificates', {method: 'POST', body: JSON.stringify(request)}),

    async accounts(): Promise<AccountStats[]> {
        const results = await pairs<AccountStats>('/api/accounts')
        return results.map(([, stats]) => stats)
    },

    async files(account: string): Promise<string[]> {
        const response = await call<ProtocolResponse>(`/api/accounts/${encode(account)}/files`)
        return response.keys ?? []
    },

    fileStats: (account: string, file: string): Promise<FileStats> =>
        record<FileStats>(`/api/accounts/${encode(account)}/files/${encode(file)}`),
}
