/**
 * Every index in the selected account.
 *
 * A separate composable from `useFileIndexes` because it answers a different
 * question at a different moment: that one is "what does this file have and
 * what shall I do to it", read on every file selection; this one is "which of
 * the forty indexes in this account is worth my attention", read on every
 * account selection and never per file.
 *
 * Not on the poll loop. The listing walks every file in the account, and a
 * verdict that changes on a flush does not need a five-second refresh - so it
 * is read when the account is chosen and again after anything that could have
 * changed one.
 */

import {ref, watch, type Ref} from 'vue'
import {useAlerts} from '@shared/composables/useAlerts'
import {accountsApi} from '../api'
import type {IndexStats} from '../types'

export function useAccountIndexes(account: Ref<string | null>) {
    const alerts = useAlerts()
    const indexes = ref<IndexStats[]>([])
    const loaded = ref(false)

    async function load(): Promise<void> {
        const selected = account.value
        if (!selected) {
            indexes.value = []
            loaded.value = false
            return
        }
        try {
            indexes.value = await accountsApi.accountIndexes(selected)
        } catch (cause) {
            // The rest of the page is still true, so this is a failure to read
            // one panel rather than a failure of the tab.
            indexes.value = []
            alerts.fail(cause)
        } finally {
            loaded.value = true
        }
    }

    watch(
        account,
        () => {
            loaded.value = false
            indexes.value = []
            void load()
        },
        {immediate: true},
    )

    return {indexes, loaded, load}
}
