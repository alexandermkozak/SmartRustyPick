/**
 * The remote protocol's response envelope.
 *
 * Every dashboard endpoint is one protocol command, so every successful reply
 * has this shape whatever the feature asked for. The payload types themselves
 * belong to the feature that reads them - see `features/<name>/types.ts`.
 *
 * Only `status` is always there: the server omits a field it did not populate.
 * Older servers sent it as `null` instead, so both spellings mean the same
 * thing here and every reader has to treat them alike.
 */
export interface ProtocolResponse<Record = unknown, Result = unknown> {
    status: string
    message?: string | null
    record?: Record | null
    results?: Result[] | null
    keys?: string[] | null
    count?: number | null
}
