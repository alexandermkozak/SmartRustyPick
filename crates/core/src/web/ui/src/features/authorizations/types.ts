/** What `LIST.CONNS` reports about each authorized client. */
export interface ClientInfo {
    thumbprint: string
    accounts: string[]
    is_admin: boolean
    /** Wire names, with ADMIN already expanded to the full set by the server. */
    capabilities: string[]
    /**
     * When the certificate expires, RFC 3339 UTC, and how many whole days that
     * is from now. Both null for a client authorized by thumbprint alone — the
     * database never saw that certificate and will not invent a date for it.
     */
    expires_at: string | null
    expires_in_days: number | null
}

/** An authorization name paired with its details. */
export interface ClientEntry {
    name: string
    info: ClientInfo
}

/** The fields `AUTHORIZE.CONN` needs. */
export interface AuthorizationRequest {
    name: string
    thumbprint: string
    accounts: string[]
    is_admin: boolean
    capabilities: string[]
}
