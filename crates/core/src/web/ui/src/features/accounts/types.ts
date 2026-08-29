/** One account, summarised without reading any of its records. */
export interface AccountStats {
    name: string
    directory: string
    file_count: number
    record_count: number
    disk_bytes: number
}

/**
 * One file's statistics. Deliberately record free: the dashboard navigates
 * files, it does not browse their contents.
 */
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
