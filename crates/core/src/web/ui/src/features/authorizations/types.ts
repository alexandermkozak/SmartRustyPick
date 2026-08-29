/** What `LIST.CONNS` reports about each authorized client. */
export interface ClientInfo {
    thumbprint: string
    accounts: string[]
    is_admin: boolean
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
}
