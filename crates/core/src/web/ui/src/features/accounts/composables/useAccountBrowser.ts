/**
 * Navigating accounts, their files and one file's statistics, and creating or
 * dropping either.
 *
 * Three levels of selection with rules between them - picking an account clears
 * the file, an account that disappears clears everything - which is exactly the
 * state a template should not be juggling inline. Creating and dropping belong
 * here for the same reason: each one changes what the selection above it is
 * still pointing at, and a page that only re-read one of the three lists would
 * leave the others describing something that is gone.
 */

import {ref, watch} from 'vue'
import {usePolling} from '@shared/composables/usePolling'
import {useAlerts} from '@shared/composables/useAlerts'
import {accountsApi} from '../api'
import type {AccountStats, FileEntry, FileStats} from '../types'

export function useAccountBrowser() {
    // Accounts and files change when someone creates or drops one, so a slow
    // poll keeps the list honest without making navigation feel busy.
    const accounts = usePolling<AccountStats[]>(accountsApi.list, {intervalMs: 20000})
    const alerts = useAlerts()

    const selectedAccount = ref<string | null>(null)
    const selectedFile = ref<string | null>(null)
    const files = ref<FileEntry[]>([])
    const filesLoaded = ref(false)
    const stats = ref<FileStats | null>(null)
    /** True while a durability change is in flight, so the button cannot be double-fired. */
    const changing = ref(false)
    /** True while an account or file is being created or dropped. */
    const maintaining = ref(false)

    function clearSelection(): void {
        selectedAccount.value = null
        selectedFile.value = null
        files.value = []
        filesLoaded.value = false
        stats.value = null
    }

    async function loadFiles(account: string): Promise<void> {
        try {
            files.value = await accountsApi.files(account)
            alerts.clear()
        } catch (cause) {
            alerts.fail(cause)
        } finally {
            filesLoaded.value = true
        }
    }

    async function selectAccount(name: string): Promise<void> {
        clearSelection()
        selectedAccount.value = name
        await loadFiles(name)
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

    /**
     * Promotes the selected file to durable writes, or demotes it back, then
     * re-reads both views of the flag from the database rather than assuming the
     * change took: the server is the one that knows, and a global
     * `durable_writes` makes every file durable whatever this page just asked for.
     */
    async function setDurable(durable: boolean): Promise<boolean> {
        const account = selectedAccount.value
        const file = selectedFile.value
        if (!account || !file || changing.value) return false
        changing.value = true
        // The reloads below report their own success, so a refusal is held and
        // raised last - otherwise the banner explaining it would be cleared by
        // the very reload that shows the file unchanged.
        let failure: unknown = null
        try {
            await accountsApi.setDurable(account, file, durable)
        } catch (cause) {
            failure = cause
        }
        try {
            await loadFiles(account)
            if (selectedFile.value === file) await selectFile(file)
        } finally {
            changing.value = false
        }
        if (failure) {
            alerts.fail(failure)
            return false
        }
        return true
    }

    /**
     * Runs one maintenance command and then re-reads what it could have
     * changed, so the page never shows a file it has just dropped or misses one
     * it has just made. The account list is polled rather than requested here;
     * a create or a drop is exactly the moment not to wait up to twenty seconds
     * for the next tick.
     */
    async function maintain(
        action: () => Promise<unknown>,
        reloadFiles: boolean,
    ): Promise<boolean> {
        if (maintaining.value) return false
        maintaining.value = true
        try {
            const done = await alerts.attempt(action)
            await accounts.refresh()
            const account = selectedAccount.value
            if (reloadFiles && account) await loadFiles(account)
            return done
        } finally {
            maintaining.value = false
        }
    }

    /**
     * Creates an account, empty or populated with the demo fixture. Neither is
     * selected afterwards: an empty one has nothing to show, and a demo account
     * is worth arriving at deliberately.
     */
    const createAccount = (name: string, demo = false): Promise<boolean> =>
        maintain(() => accountsApi.createAccount(name, demo), false)

    /** Drops an account and everything in it, clearing the selection if it was this one. */
    function deleteAccount(name: string): Promise<boolean> {
        if (selectedAccount.value === name) clearSelection()
        return maintain(() => accountsApi.deleteAccount(name), false)
    }

    async function createFile(name: string, durable: boolean): Promise<boolean> {
        const account = selectedAccount.value
        if (!account) return false
        return maintain(() => accountsApi.createFile(account, name, durable), true)
    }

    async function deleteFile(name: string): Promise<boolean> {
        const account = selectedAccount.value
        if (!account) return false
        // The statistics panel is describing a file that is about to stop
        // existing, so it goes before the request rather than after it.
        if (selectedFile.value === name) {
            selectedFile.value = null
            stats.value = null
        }
        return maintain(() => accountsApi.deleteFile(account, name), true)
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
        changing,
        maintaining,
        selectAccount,
        selectFile,
        setDurable,
        createAccount,
        deleteAccount,
        createFile,
        deleteFile,
    }
}
