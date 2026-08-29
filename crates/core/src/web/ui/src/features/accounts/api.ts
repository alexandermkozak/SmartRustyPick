import {encode, keys, pairs, record} from '@shared/api/client'
import type {AccountStats, FileStats} from './types'

export const accountsApi = {
    /** `LIST.ACCOUNTS`: every account this client may reach. */
    async list(): Promise<AccountStats[]> {
        const results = await pairs<AccountStats>('/api/accounts')
        return results.map(([, stats]) => stats)
    },

    /** `LIST.FILES` for one account. */
    files: (account: string): Promise<string[]> => keys(`/api/accounts/${encode(account)}/files`),

    /** `FILE.STATS` for one file. */
    fileStats: (account: string, file: string): Promise<FileStats> =>
        record<FileStats>(`/api/accounts/${encode(account)}/files/${encode(file)}`),
}
