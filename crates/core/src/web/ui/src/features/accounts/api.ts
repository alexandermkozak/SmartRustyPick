import {call, encode, pairs, record} from '@shared/api/client'
import type {AccountStats, FileEntry, FileStats} from './types'

export const accountsApi = {
    /** `LIST.ACCOUNTS`: every account this client may reach. */
    async list(): Promise<AccountStats[]> {
        const results = await pairs<AccountStats>('/api/accounts')
        return results.map(([, stats]) => stats)
    },

    /** `LIST.FILES` for one account, each file with its durability flag. */
    async files(account: string): Promise<FileEntry[]> {
        const results = await pairs<{durable: boolean}>(`/api/accounts/${encode(account)}/files`)
        return results.map(([name, info]) => ({name, durable: info.durable === true}))
    },

    /** `FILE.STATS` for one file. */
    fileStats: (account: string, file: string): Promise<FileStats> =>
        record<FileStats>(`/api/accounts/${encode(account)}/files/${encode(file)}`),

    /**
     * `SET.FILE`: promote a file to durable writes, or demote it back. Admin
     * only, like creating one - the database decides that, not this page.
     */
    setDurable: (account: string, file: string, durable: boolean): Promise<unknown> =>
        call(`/api/accounts/${encode(account)}/files/${encode(file)}`, {
            method: 'POST',
            body: JSON.stringify({durable}),
        }),
}
