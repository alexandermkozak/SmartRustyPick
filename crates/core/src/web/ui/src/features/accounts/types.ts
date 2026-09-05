import type {Health, HealthSummary} from '@shared/health'

/** One account, summarised without reading any of its records. */
export interface AccountStats {
    name: string
    directory: string
    file_count: number
    record_count: number
    disk_bytes: number
    /** Secondary indexes across every file in the account. Absent from an
     *  older server's reply, which is why these are optional. */
    index_count?: number
    stale_indexes?: number
    unhealthy_files?: number
    health?: HealthSummary
}

/**
 * One file as the listing describes it: its name, and whether its writes are
 * flushed before they are acknowledged.
 */
export interface FileEntry {
    name: string
    durable: boolean
    /**
     * The *cheap* verdict: section metadata and index `state` files only, no
     * group trailers and no records. Enough to say which file is worth
     * opening, which is all a listing should cost.
     */
    health: HealthSummary
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
    /** The file this index belongs to. Carried on the row rather than implied
     *  by the request, so the per-file listing and the account-wide one render
     *  through the same table. */
    file: string
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
    /** Values this index deliberately does not hold. A query for one of them
     *  falls back to a scan rather than trusting an empty posting list. */
    excluded: string[]
    usage: IndexUsage
    health: Health
}

/**
 * What the read path has asked of one index since the server started. Never
 * persisted, and reset by a restart - the question these answer is "is anything
 * querying this", and a count carried over from a previous run answers it
 * wrongly.
 */
export interface IndexUsage {
    lookups: number
    candidates: number
    matched: number
    /** Lookups whose survivors could be attributed to this index, which is a
     *  query one index resolved on its own. */
    measured_lookups: number
    /** Lookups that fell back to a scan because the value asked for is one this
     *  index excludes. */
    excluded_lookups: number
}

/** One value of an index and how many record keys carry it. */
export interface IndexValue {
    value: string
    keys: number
}

/**
 * `INDEX.STATS`: one index in full, with the values that dominate it.
 *
 * The histogram is what turns "this index is skewed" into "STATUS = ACTIVE is
 * 91% of it" - and the value it names is the one to exclude.
 */
export interface IndexReport {
    record_count: number
    index: IndexStats
    top_values: IndexValue[]
    /** False when the values could not be read: a stale index, or a section
     *  that would not load. `top_values` is then empty, which is not the same
     *  as an empty index and is why the flag is here. */
    values_available: boolean
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
    /** Absent from an older server's reply, which is why these are optional. */
    indexes?: IndexStats[]

    // Derived measures. All of it comes from the section metadata and the group
    // trailers; none of it reads a record.
    /** Bytes in the record groups alone; `disk_bytes` is the whole directory. */
    group_bytes?: number
    index_bytes?: number
    group_records?: GroupDistribution
    records_per_group_target?: number
    load_factor?: number
    records_until_growth?: number
    records_until_shrink?: number | null
    largest_group_share?: number
    /** Largest group over the mean, in records. Scale-free, so it reads the
     *  same on a file of any size. */
    skew?: number
    health?: Health
}

/**
 * How records are spread over a section's groups.
 *
 * Over the *modulus* rather than the group files: a group holding nothing has
 * no file at all, so averaging the files that exist would report a file whose
 * records had piled into four groups out of thirty-two as perfectly even.
 */
export interface GroupDistribution {
    groups: number
    min: number
    max: number
    mean: number
    median: number
    empty: number
    /** Groups above twice the mean. Says what one extreme cannot: that the
     *  hash is not spreading, rather than that one group is unlucky. */
    overweight: number
    /** Groups written before the format appended a trailer, whose counts could
     *  not be read. Counted rather than folded in as zero. */
    unreadable: number
    buckets: DistributionBucket[]
}

export interface DistributionBucket {
    min: number
    max: number
    groups: number
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
    /** The controlling field this entry's values pair with, empty when it is in no group. */
    association: string
    /** `V` (value for value) or `S` (sub-value for sub-value); empty without an association. */
    associationDepth: string
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
    association: string
    associationDepth: string
    conversion: string
}
