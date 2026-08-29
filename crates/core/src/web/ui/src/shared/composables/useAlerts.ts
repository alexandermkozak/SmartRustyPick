/**
 * The one-line banner under the tabs.
 *
 * Errors from an action the operator just took - revoking a client, issuing a
 * certificate - belong somewhere they cannot be missed, which is not inside the
 * view that may be about to re-render. Shared at module scope so any view can
 * raise one and `App.vue` is the only thing that renders it.
 */

import {readonly, ref} from 'vue'
import {ApiError} from '@shared/api/client'

const message = ref<string | null>(null)

export function useAlerts() {
    return {
        message: readonly(message),

        /** Shows an error, taking the message an `ApiError` already carries. */
        fail(cause: unknown): void {
            message.value =
                cause instanceof ApiError
                    ? cause.message
                    : cause instanceof Error
                      ? cause.message
                      : String(cause)
        },

        clear(): void {
            message.value = null
        },

        /**
         * Runs an action, reporting a failure in the banner and reporting whether
         * it succeeded. Views use the result to decide whether to reload.
         */
        async attempt(action: () => Promise<unknown>): Promise<boolean> {
            try {
                await action()
                message.value = null
                return true
            } catch (cause) {
                this.fail(cause)
                return false
            }
        },
    }
}
