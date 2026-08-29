/**
 * The server snapshot, polled once for the whole application.
 *
 * The header and the overview both want it, and a second poller would double
 * the load for the same numbers. So the poller is built at module scope, owned
 * by this slice rather than by any component.
 *
 * Consumers are reference counted: the poll starts when the first component
 * that wants it mounts and stops when the last one goes away. That keeps the
 * lifetime a detail of this slice - the shell does not have to start a poll on
 * behalf of a feature it otherwise knows nothing about, and no view can stop
 * another view's data by unmounting.
 */

import {getCurrentInstance, onMounted, onUnmounted} from 'vue'
import {usePolling} from '@shared/composables/usePolling'
import {overviewApi} from '../api'
import type {ServerSnapshot} from '../types'

const stats = usePolling<ServerSnapshot>(overviewApi.stats, {intervalMs: 5000, immediate: false})

let consumers = 0

export function useServerStats() {
    if (getCurrentInstance()) {
        onMounted(() => {
            if (++consumers === 1) stats.start()
        })
        onUnmounted(() => {
            if (--consumers === 0) stats.stop()
        })
    }
    return stats
}
