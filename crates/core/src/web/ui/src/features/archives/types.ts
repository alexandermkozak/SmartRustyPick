/** What the archive endpoints hand back. */

/** The scope an export covers, as the form holds it. */
export interface ExportScope {
    /** Empty for every account. */
    account: string
    /** Empty for the whole account. Only meaningful with an account. */
    file: string
}

/** What a restore was asked to do. */
export interface ImportOptions {
    /** Restore into this account instead of the one the archive names. */
    into: string
    /** Replace files that are already there. */
    overwrite: boolean
    /** Report and write nothing. */
    verify: boolean
}

/** One file's fate in a restore. */
export interface ImportedFile {
    account: string
    file: string
    /** `created` or `replaced` - or what would happen, on a verify. */
    action: string
    records: number
    dictionary: number
    bytes: number
}

/** The `archive` object an `IMPORT.BYTES` reply carries. */
export interface ImportReport {
    source: string
    taken: number
    takenUtc: string
    archiveFormat: number
    storageFormat: number
    dryRun: boolean
    accountsCreated: string[]
    files: ImportedFile[]
    records: number
}
