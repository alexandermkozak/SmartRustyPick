/**
 * One file's secondary indexes: what it has, and the three things an operator
 * does to them.
 *
 * Here rather than in a component for the same reason the dictionary is: the
 * list belongs to the selected file, so choosing a different file has to
 * abandon whatever was half chosen about the old one, and every change has to
 * be followed by a re-read - creating or rebuilding an index changes the
 * numbers that say whether it was worth creating.
 *
 * Nothing here decides whether a field *can* be indexed. `CREATE.INDEX` does
 * that, and a second opinion in the browser is a rule that can disagree with
 * the database - so a refusal is reported as the database worded it.
 */

import {computed, ref, watch, type Ref} from 'vue'
import {useAlerts} from '@shared/composables/useAlerts'
import {accountsApi} from '../api'
import type {DictionaryEntry, IndexReport, IndexStats} from '../types'

export function useFileIndexes(
    account: Ref<string | null>,
    file: Ref<string | null>,
    dictionary: Ref<DictionaryEntry[]>,
) {
    const alerts = useAlerts()

    const indexes = ref<IndexStats[]>([])
    const loaded = ref(false)
    /** True while a create, rebuild or drop is in flight. */
    const working = ref(false)
    /** The field the create form has selected. */
    const field = ref('')
    /** The index whose values are open, and the report behind them. `null`
     *  while nothing is open, which is most of the time - reading an index's
     *  values costs more than listing them, so it is asked for deliberately. */
    const inspecting = ref<string | null>(null)
    const report = ref<IndexReport | null>(null)

    /**
     * The dictionary fields that could still be indexed: everything defined,
     * minus what is indexed already, minus `ID` - which is the record key and
     * is found in one hash lookup without any index at all.
     */
    const candidates = computed(() => {
        const indexed = new Set(indexes.value.map((index) => index.field))
        return dictionary.value
            .filter((entry) => entry.name !== 'ID' && !indexed.has(entry.name))
            .map((entry) => entry.name)
    })

    async function load(): Promise<void> {
        const [selectedAccount, selectedFile] = [account.value, file.value]
        if (!selectedAccount || !selectedFile) {
            indexes.value = []
            loaded.value = false
            return
        }
        try {
            indexes.value = await accountsApi.indexes(selectedAccount, selectedFile)
            alerts.clear()
        } catch (cause) {
            indexes.value = []
            alerts.fail(cause)
        } finally {
            loaded.value = true
        }
    }

    /**
     * Reads one index's values, or forgets them when nothing is open.
     *
     * Its own request rather than part of the listing: `LIST.INDEXES` is read
     * on every navigation and stays cheap, while this sorts the index's values.
     */
    async function inspect(name: string | null): Promise<void> {
        inspecting.value = name
        report.value = null
        const [selectedAccount, selectedFile] = [account.value, file.value]
        if (!name || !selectedAccount || !selectedFile) return
        try {
            report.value = await accountsApi.indexReport(selectedAccount, selectedFile, name)
        } catch (cause) {
            // The listing is still on screen and still true, so this is a
            // failure to read *more*, not a failure of the page.
            alerts.fail(cause)
        }
    }

    /**
     * Runs one change and re-reads the list afterwards rather than assuming what
     * landed: every one of these moves the counts, and a rebuild's whole purpose
     * is to change what the `stale` column says.
     *
     * The reload reports its own success, so a refusal is held and raised after
     * it - otherwise the banner explaining the refusal would be cleared by the
     * very reload that shows the list unchanged.
     */
    async function change(action: () => Promise<unknown>): Promise<boolean> {
        const [selectedAccount, selectedFile] = [account.value, file.value]
        if (!selectedAccount || !selectedFile || working.value) return false
        working.value = true
        let failure: unknown = null
        try {
            await action()
        } catch (cause) {
            failure = cause
        }
        try {
            await load()
        } finally {
            working.value = false
        }
        if (failure) {
            alerts.fail(failure)
            return false
        }
        return true
    }

    /** Indexes the selected field, clearing the choice once the database has it. */
    async function create(name?: string): Promise<boolean> {
        const [selectedAccount, selectedFile] = [account.value, file.value]
        const chosen = (name ?? field.value).trim()
        if (!selectedAccount || !selectedFile || !chosen) return false
        const done = await change(() =>
            accountsApi.createIndex(selectedAccount, selectedFile, chosen),
        )
        if (done) field.value = ''
        return done
    }

    async function rebuild(name: string): Promise<boolean> {
        const [selectedAccount, selectedFile] = [account.value, file.value]
        if (!selectedAccount || !selectedFile) return false
        return change(() => accountsApi.rebuildIndex(selectedAccount, selectedFile, name))
    }

    async function remove(name: string): Promise<boolean> {
        const [selectedAccount, selectedFile] = [account.value, file.value]
        if (!selectedAccount || !selectedFile) return false
        if (inspecting.value === name) await inspect(null)
        return change(() => accountsApi.deleteIndex(selectedAccount, selectedFile, name))
    }

    /**
     * Replaces the values one index skips.
     *
     * The values are re-read afterwards as well as the list: excluding a value
     * rebuilds the index, so the histogram the operator was looking at when
     * they pressed the button is the thing that has just changed.
     */
    async function exclude(name: string, values: string[]): Promise<boolean> {
        const [selectedAccount, selectedFile] = [account.value, file.value]
        if (!selectedAccount || !selectedFile) return false
        const done = await change(() =>
            accountsApi.setIndexExclusions(selectedAccount, selectedFile, name, values),
        )
        if (inspecting.value === name) await inspect(name)
        return done
    }

    // A different file has different indexes, and whatever field was chosen for
    // the old one means nothing on the new one.
    watch(
        [account, file],
        () => {
            loaded.value = false
            indexes.value = []
            field.value = ''
            inspecting.value = null
            report.value = null
            void load()
        },
        {immediate: true},
    )

    // A field that has just been added to the dictionary is a field that can be
    // indexed; one that has just been removed is not. Keeping the choice inside
    // the candidates means the form never offers a field that is no longer there.
    watch(candidates, (available) => {
        if (field.value && !available.includes(field.value)) field.value = ''
    })

    return {
        indexes,
        loaded,
        working,
        field,
        candidates,
        inspecting,
        report,
        load,
        create,
        rebuild,
        remove,
        inspect,
        exclude,
    }
}
