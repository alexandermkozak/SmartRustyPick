import {record} from '@shared/api/client'
import type {ServerSnapshot} from './types'

export const overviewApi = {
    /** `SERVER.STATS`: uptime, totals and the sessions open right now. */
    stats: (): Promise<ServerSnapshot> => record<ServerSnapshot>('/api/stats'),
}
