/**
 * The wire shapes, mirroring the Rust types they are serialized from.
 *
 * `ServerSnapshot` and `ConnectionSnapshot` come from `server/stats.rs`,
 * `AccountStats` and `FileStats` from `db/models.rs`, and the envelope from
 * `server/models.rs`. Keeping the names identical means a field renamed in Rust
 * shows up here as a type error rather than as `undefined` on screen.
 */

/** One connection currently held open by a client. */
export interface ConnectionSnapshot {
    id: number
    peer: string
    client_name: string
    thumbprint: string
    is_admin: boolean
    connected_seconds: number
    requests: number
    last_command: string
    idle_seconds: number
}

/** The `SERVER.STATS` record: the server's own view of what it is doing. */
export interface ServerSnapshot {
    uptime_seconds: number
    started_at: number
    listen_addr: string
    total_connections: number
    rejected_connections: number
    total_requests: number
    failed_requests: number
    active_connections: ConnectionSnapshot[]
    /** Merged in by the command handler from the engine side. */
    pending_writes: number
    loaded_tables: number
    authorized_clients: number
}

/** One authorized client, as `LIST.CONNS` reports it. */
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

/** One account, summarised without reading any of its records. */
export interface AccountStats {
    name: string
    directory: string
    file_count: number
    record_count: number
    disk_bytes: number
}

/** One file's statistics. Deliberately record free. */
export interface FileStats {
    account: string
    name: string
    record_count: number
    dict_count: number
    modulus: number
    version: number
    group_count: number
    smallest_group_bytes: number
    largest_group_bytes: number
    disk_bytes: number
    checksums: boolean
    legacy: boolean
    durable: boolean
    loaded: boolean
    modified_seconds_ago: number | null
}

/** A freshly issued certificate, returned once and never stored by the page. */
export interface GeneratedCert {
    common_name: string
    thumbprint: string
    certificate_pem: string
    private_key_pem: string
    ca_pem: string
    cert_path: string
    key_path: string
    pfx_path: string | null
}

/** The protocol's response envelope, as the dashboard's API passes it through. */
export interface ProtocolResponse<Record = unknown, Result = unknown> {
    status: string
    message: string | null
    record: Record | null
    results: Result[] | null
    keys: string[] | null
    count: number | null
}
