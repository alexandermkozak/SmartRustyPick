/** One account, summarised without reading any of its records. */
export interface AccountStats {
    name: string
    directory: string
    file_count: number
    record_count: number
    disk_bytes: number
}

/**
 * One file as the listing describes it: its name, and whether its writes are
 * flushed before they are acknowledged.
 */
export interface FileEntry {
    name: string
    durable: boolean
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

/**
 * One dictionary entry as `LIST.DICT` decomposes it.
 *
 * `field` and `width` are `null` for an entry that holds something other than a
 * number in those positions; `definition` is the raw display string, which is
 * the whole truth about an entry however unusual it is.
 */
export interface DictionaryEntry {
    name: string
    field: number | null
    heading: string
    justification: string
    width: number | null
    conversion: string
    definition: string
}

/**
 * The dictionary form's contents. Everything is a string because that is what
 * an input holds; `SET.DICT` accepts numbers spelled either way and is the one
 * place the attributes are judged, so the page does not second-guess them.
 */
export interface DictionaryDraft {
    name: string
    field: string
    heading: string
    justification: string
    width: string
    conversion: string
}
