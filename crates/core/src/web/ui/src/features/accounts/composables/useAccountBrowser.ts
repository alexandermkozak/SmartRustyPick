/**
 * Navigating accounts, their files and one file's statistics.
 *
 * Three levels of selection with rules between them - picking an account clears
 * the file, an account that disappears clears everything - which is exactly the
 * state a template should not be juggling inline.
 */

import {ref, watch} from 'vue'
import {usePolling} from '@shared/composables/usePolling'
import {useAlerts} from '@shared/composables/useAlerts'
import {accountsApi} from '../api'
import type {AccountStats, FileStats} from '../types'

export function useAccountBrowser() {
    // Accounts and files change when someone creates or drops one, so a slow
    // poll keeps the list honest without making navigation feel busy.
    const accounts = usePolling<AccountStats[]>(accountsApi.list, {intervalMs: 20000})
    const alerts = useAlerts()

    const selectedAccount = ref<string | null>(null)
    const selectedFile = ref<string | null>(null)
    const files = ref<string[]>([])
    const filesLoaded = ref(false)
    const stats = ref<FileStats | null>(null)

    function clearSelection(): void {
        selectedAccount.value = null
        selectedFile.value = null
        files.value = []
        filesLoaded.value = false
        stats.value = null
    }

    async function selectAccount(name: string): Promise<void> {
        clearSelection()
        selectedAccount.value = name
        try {
            files.value = await accountsApi.files(name)
            alerts.clear()
        } catch (cause) {
            alerts.fail(cause)
        } finally {
            filesLoaded.value = true
        }
    }

    async function selectFile(file: string): Promise<void> {
        if (!selectedAccount.value) return
        selectedFile.value = file
        try {
            stats.value = await accountsApi.fileStats(selectedAccount.value, file)
            alerts.clear()
        } catch (cause) {
            stats.value = null
            alerts.fail(cause)
        }
    }

    // An account dropped by someone else should not leave a dead selection behind.
    watch(accounts.data, (list) => {
        if (!list || !selectedAccount.value) return
        if (!list.some((account) => account.name === selectedAccount.value)) clearSelection()
    })

    return {
        accounts,
        selectedAccount,
        selectedFile,
        files,
        filesLoaded,
        stats,
        selectAccount,
        selectFile,
    }
}
