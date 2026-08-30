import {call, encode, pairs, record} from '@shared/api/client'
import type {AccountStats, DictionaryDraft, DictionaryEntry, FileEntry, FileStats} from './types'

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

    /** `CREATE.ACCOUNT`. Admin only. */
    createAccount: (name: string): Promise<unknown> =>
        call('/api/accounts', {method: 'POST', body: JSON.stringify({name})}),

    /** `DELETE.ACCOUNT`: the account and every file in it. Admin only. */
    deleteAccount: (name: string): Promise<unknown> =>
        call(`/api/accounts/${encode(name)}`, {method: 'DELETE'}),

    /** `CREATE.FILE`, optionally durable from the start. Admin only. */
    createFile: (account: string, name: string, durable: boolean): Promise<unknown> =>
        call(`/api/accounts/${encode(account)}/files`, {
            method: 'POST',
            body: JSON.stringify({name, durable}),
        }),

    /** `DELETE.FILE`: the file, its records and its dictionary. Admin only. */
    deleteFile: (account: string, file: string): Promise<unknown> =>
        call(`/api/accounts/${encode(account)}/files/${encode(file)}`, {method: 'DELETE'}),

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
