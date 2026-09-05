import {call, encode, pairs, record} from '@shared/api/client'
import {verdictOf, type HealthSummary} from '@shared/health'
import type {
    AccountStats,
    DictionaryDraft,
    DictionaryEntry,
    FileEntry,
    FileStats,
    IndexReport,
    IndexStats,
    QueueDraft,
} from './types'

/**
 * A listing's health summary, tolerating a server that sends none.
 *
 * An older server's `LIST.FILES` reply has no `health` at all, and treating
 * that as a problem would be worse than treating it as nothing to report.
 */
function summaryOf(value: unknown, reasons: unknown): HealthSummary {
    return {
        verdict: verdictOf(value),
        reasons: Array.isArray(reasons)
            ? reasons.filter((r): r is string => typeof r === 'string')
            : [],
    }
}

export const accountsApi = {
    /** `LIST.ACCOUNTS`: every account this client may reach. */
    async list(): Promise<AccountStats[]> {
        const results = await pairs<AccountStats>('/api/accounts')
        return results.map(([, stats]) => stats)
    },

    /**
     * `LIST.FILES` for one account, each file with its durability flag and its
     * cheap health verdict - so a problem file is findable from the list rather
     * than only by opening every file in turn.
     */
    async files(account: string): Promise<FileEntry[]> {
        const results = await pairs<{
            durable: boolean
            queue?: boolean
            health?: string
            health_reasons?: string[]
        }>(`/api/accounts/${encode(account)}/files`)
        return results.map(([name, info]) => ({
            name,
            durable: info.durable === true,
            queue: info.queue === true,
            health: summaryOf(info.health, info.health_reasons),
        }))
    },

    /** `FILE.STATS` for one file. */
    fileStats: (account: string, file: string): Promise<FileStats> =>
        record<FileStats>(`/api/accounts/${encode(account)}/files/${encode(file)}`),

    /**
     * `SET.FILE`: change what an existing file is - durable or buffered, a queue
     * or an ordinary file, and a queue's claim policy. Admin only, like
     * creating one; the database decides that, not this page.
     *
     * Only the fields given are sent, because the database leaves an omitted
     * attribute alone: a change of durability must not quietly stop a file
     * being a queue.
     */
    setFile: (
        account: string,
        file: string,
        changes: {durable?: boolean; queue?: boolean} & QueueDraft,
    ): Promise<unknown> =>
        call(`/api/accounts/${encode(account)}/files/${encode(file)}`, {
            method: 'POST',
            body: JSON.stringify(changes),
        }),

    /**
     * `CREATE.ACCOUNT`, or `CREATE.TEST.ACCOUNT` for the demo fixture. Admin
     * only, and one endpoint for both: the page is asking for an account either
     * way, and only the contents differ.
     */
    createAccount: (name: string, demo: boolean): Promise<unknown> =>
        call('/api/accounts', {method: 'POST', body: JSON.stringify({name, demo})}),

    /** `DELETE.ACCOUNT`: the account and every file in it. Admin only. */
    deleteAccount: (name: string): Promise<unknown> =>
        call(`/api/accounts/${encode(name)}`, {method: 'DELETE'}),

    /**
     * `CREATE.FILE`, optionally durable, and optionally a queue with its claim
     * policy. Admin only.
     *
     * The policy travels with the create rather than following it, so a queue is
     * never briefly running on a timeout nobody asked for.
     */
    createFile: (
        account: string,
        name: string,
        durable: boolean,
        queue?: QueueDraft | null,
    ): Promise<unknown> =>
        call(`/api/accounts/${encode(account)}/files`, {
            method: 'POST',
            body: JSON.stringify(queue ? {name, durable, queue: true, ...queue} : {name, durable}),
        }),

    /** `DELETE.FILE`: the file, its records and its dictionary. Admin only. */
    deleteFile: (account: string, file: string): Promise<unknown> =>
        call(`/api/accounts/${encode(account)}/files/${encode(file)}`, {method: 'DELETE'}),

    /** `LIST.INDEXES`: every secondary index of one file, with its statistics. */
    async indexes(account: string, file: string): Promise<IndexStats[]> {
        const results = await pairs<IndexStats>(
            `/api/accounts/${encode(account)}/files/${encode(file)}/indexes`,
        )
        return results.map(([, stats]) => stats)
    },

    /** `LIST.INDEXES` with no file: every index in the account, so index health
     *  is visible without walking file by file. */
    async accountIndexes(account: string): Promise<IndexStats[]> {
        const results = await pairs<IndexStats>(`/api/accounts/${encode(account)}/indexes`)
        return results.map(([, stats]) => stats)
    },

    /** `INDEX.STATS`: one index in full, with the values that dominate it. */
    indexReport: (account: string, file: string, field: string, limit = 10): Promise<IndexReport> =>
        record<IndexReport>(
            `/api/accounts/${encode(account)}/files/${encode(file)}/indexes/${encode(field)}` +
                `?limit=${encodeURIComponent(limit)}`,
        ),

    /**
     * `SET.INDEX.EXCLUDE`: replace the values one index does not hold, and
     * rebuild it. An empty list clears the exclusions. Admin only.
     */
    setIndexExclusions: (
        account: string,
        file: string,
        field: string,
        values: string[],
    ): Promise<unknown> =>
        call(
            `/api/accounts/${encode(account)}/files/${encode(file)}/indexes/${encode(field)}/exclude`,
            {method: 'POST', body: JSON.stringify({values})},
        ),

    /** `CREATE.INDEX` on one dictionary field, optionally skipping values that
     *  are not worth indexing. Admin only. */
    createIndex: (
        account: string,
        file: string,
        field: string,
        exclude: string[] = [],
    ): Promise<unknown> =>
        call(`/api/accounts/${encode(account)}/files/${encode(file)}/indexes`, {
            method: 'POST',
            body: JSON.stringify({field, values: exclude}),
        }),

    /** `REBUILD.INDEX`: derive an existing index from the records again. Admin only. */
    rebuildIndex: (account: string, file: string, field: string): Promise<unknown> =>
        call(
            `/api/accounts/${encode(account)}/files/${encode(file)}/indexes/${encode(field)}/rebuild`,
            {method: 'POST'},
        ),

    /** `DELETE.INDEX`: drop an index and its section. The records stay. Admin only. */
    deleteIndex: (account: string, file: string, field: string): Promise<unknown> =>
        call(`/api/accounts/${encode(account)}/files/${encode(file)}/indexes/${encode(field)}`, {
            method: 'DELETE',
        }),

    /** `LIST.DICT`: every dictionary entry of one file, in attribute order. */
    async dictionary(account: string, file: string): Promise<DictionaryEntry[]> {
        const results = await pairs<Omit<DictionaryEntry, 'name'>>(
            `/api/accounts/${encode(account)}/files/${encode(file)}/dictionary`,
        )
        return results.map(([name, entry]) => ({name, ...entry}))
    },

    /** `SET.DICT`: create or replace one entry. The database judges the attributes. */
    saveDictionaryEntry: (
        account: string,
        file: string,
        draft: DictionaryDraft,
    ): Promise<unknown> =>
        call(`/api/accounts/${encode(account)}/files/${encode(file)}/dictionary`, {
            method: 'POST',
            body: JSON.stringify(draft),
        }),

    /** `DELETE` with `is_dict`: remove one entry. */
    deleteDictionaryEntry: (account: string, file: string, name: string): Promise<unknown> =>
        call(`/api/accounts/${encode(account)}/files/${encode(file)}/dictionary/${encode(name)}`, {
            method: 'DELETE',
        }),
}
