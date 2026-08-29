/**
 * A value re-fetched on an interval, for views that watch something live.
 *
 * Four behaviours are what make this worth a composable rather than a
 * `setInterval` in each view:
 *
 * - Requests never overlap. The next tick is scheduled after the previous
 *   response lands, so a slow server produces a slower refresh rate instead of
 *   a growing pile of in-flight requests.
 * - Polling stops while the tab is hidden. A dashboard left open on a second
 *   monitor should not keep a database busy answering questions nobody reads.
 * - A refresh that fails keeps the last good data on screen and reports the
 *   error beside it, rather than blanking a view that was working a moment ago.
 * - Lifecycle is bound to the calling component only when there is one. Called
 *   at module scope - as the overview slice's `useServerStats` does, to share
 *   one poll across views - the caller drives `start` and `stop` instead, so
 *   unmounting the view that happened to be first does not stop everyone
 *   else's data.
 */

import {getCurrentInstance, onMounted, onUnmounted, readonly, ref, shallowRef} from 'vue'
import {ApiError} from '@shared/api/client'

export interface PollingOptions {
    /** Milliseconds between refreshes. */
    intervalMs?: number
    /** Start polling as soon as the calling component mounts. */
    immediate?: boolean
}

export function usePolling<T>(load: () => Promise<T>, options: PollingOptions = {}) {
    const {intervalMs = 5000, immediate = true} = options

    const data = shallowRef<T | null>(null)
    const error = ref<string | null>(null)
    const loading = ref(false)
    /** Whether the interval is running; views expose this as a Live toggle. */
    const live = ref(false)
    /** True once a response has landed, so a view can tell empty from not-yet. */
    const loaded = ref(false)

    let timer: number | null = null
    let listening = false
    let disposed = false

    const clearTimer = () => {
        if (timer !== null) {
            window.clearTimeout(timer)
            timer = null
        }
    }

    const schedule = () => {
        clearTimer()
        if (disposed || !live.value || document.visibilityState === 'hidden') return
        timer = window.setTimeout(() => void refresh(), intervalMs)
    }

    async function refresh(): Promise<void> {
        if (loading.value || disposed) return
        loading.value = true
        try {
            const value = await load()
            if (disposed) return
            data.value = value
            loaded.value = true
            error.value = null
        } catch (cause) {
            if (disposed) return
            error.value = cause instanceof ApiError ? cause.message : String(cause)
            // An expired session will not fix itself, and retrying every few seconds
            // would only fill the server's log with refusals.
            if (cause instanceof ApiError && cause.unauthorized) {
                live.value = false
                clearTimer()
            }
        } finally {
            loading.value = false
            schedule()
        }
    }

    // Resume as soon as the tab is looked at again, with an immediate refresh so
    // the first thing seen is current rather than however old the last tick was.
    const onVisibilityChange = () => {
        if (document.visibilityState === 'visible' && live.value) void refresh()
        else clearTimer()
    }

    function start(): void {
        if (disposed) return
        if (!listening) {
            document.addEventListener('visibilitychange', onVisibilityChange)
            listening = true
        }
        live.value = true
        void refresh()
    }

    function stop(): void {
        live.value = false
        clearTimer()
        if (listening) {
            document.removeEventListener('visibilitychange', onVisibilityChange)
            listening = false
        }
    }

    // Only a poller owned by a component follows that component's lifetime.
    if (getCurrentInstance()) {
        onMounted(() => {
            if (immediate) start()
        })
        onUnmounted(() => {
            disposed = true
            stop()
        })
    }

    return {
        data,
        error: readonly(error),
        loading: readonly(loading),
        loaded: readonly(loaded),
        live: readonly(live),
        refresh,
        start,
        stop,
    }
}

export type Polling<T> = ReturnType<typeof usePolling<T>>
