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
 * One secondary index, as `LIST.INDEXES` describes it.
 *
 * The three counts are what the decision is made on. `values` against the
 * file's record count is how selective the field is; `postings` is what
 * maintaining the index costs per write; `largest_postings` is the skew the
 * average hides - an index whose biggest value covers half the file saves
 * nothing on that value.
 */
export interface IndexStats {
    field: string
    attribute: number
    values: number
    postings: number
    largest_postings: number
    modulus: number
    version: number
    group_count: number
    disk_bytes: number
    data_version: number
    stale: boolean
    loaded: boolean
    built_seconds_ago: number | null
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
    /** Absent from an older server's reply, which is why it is optional here. */
    indexes?: IndexStats[]
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
