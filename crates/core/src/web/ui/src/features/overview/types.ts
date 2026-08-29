/**
 * What the overview reads: the server's own view of itself.
 *
 * Mirrors `ConnectionSnapshot` and `ServerSnapshot` in `crates/core/src/server/stats.rs`,
 * plus the three engine figures `SERVER.STATS` merges into the same object.
 * Keeping the names identical means a field renamed in Rust shows up here as a
 * type error rather than as `undefined` on screen.
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

/** The `SERVER.STATS` record. */
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
