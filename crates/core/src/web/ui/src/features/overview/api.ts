import {pairs, record} from '@shared/api/client'
import type {AccountHealth, ServerSnapshot} from './types'

export const overviewApi = {
    /** `SERVER.STATS`: uptime, totals and the sessions open right now. */
    stats: (): Promise<ServerSnapshot> => record<ServerSnapshot>('/api/stats'),

    /**
     * `LIST.ACCOUNTS`, read for its health roll-up rather than its sizes.
     *
     * Not part of `SERVER.STATS`, and not on the poll loop: the roll-up walks
     * every account's files, which is affordable once on arrival and not every
     * few seconds. The point of putting it here at all is that nobody has to
     * remember to go and check.
     */
    async storage(): Promise<AccountHealth[]> {
        const results = await pairs<AccountHealth>('/api/accounts')
        return results.map(([, stats]) => stats)
    },
}
