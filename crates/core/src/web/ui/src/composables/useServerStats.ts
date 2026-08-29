/**
 * The server snapshot, polled once for the whole application.
 *
 * The header and the overview both want it, and a second poller would double
 * the load for the same numbers. Built at module scope so it belongs to no
 * single component: `App.vue` starts it and stops it, and views come and go
 * around it without interrupting the poll.
 */

import {api} from '../api'
import {usePolling} from './usePolling'
import type {ServerSnapshot} from '../types'

const stats = usePolling<ServerSnapshot>(api.stats, {intervalMs: 5000, immediate: false})

export function useServerStats() {
    return stats
}
