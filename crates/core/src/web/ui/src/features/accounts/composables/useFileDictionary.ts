/**
 * One file's dictionary: the entries it holds, and the form that edits them.
 *
 * The dictionary is the only part of a file's contents the dashboard touches,
 * and it is here rather than in a component because two pieces of state have a
 * rule between them: the form is either adding an entry or replacing a named
 * one, and the file underneath can change while it is open. Selecting a
 * different file has to abandon the edit, not carry it across.
 *
 * Nothing here validates an attribute number or a justification. `SET.DICT`
 * does that, and a second opinion in the browser is a rule that can disagree
 * with the database - so a refusal is reported as the database worded it.
 */

import {computed, ref, watch, type Ref} from 'vue'
import {useAlerts} from '@shared/composables/useAlerts'
import {accountsApi} from '../api'
import type {DictionaryDraft, DictionaryEntry} from '../types'

/** An empty form: a new entry, left-justified, with no conversion. */
const blankDraft = (): DictionaryDraft => ({
    name: '',
    field: '',
    heading: '',
    justification: 'L',
    association: '',
    associationDepth: '',
    conversion: '',
    width: '',
})

export function useFileDictionary(account: Ref<string | null>, file: Ref<string | null>) {
    const alerts = useAlerts()

    const entries = ref<DictionaryEntry[]>([])
    const loaded = ref(false)
    /** True while a save or a removal is in flight. */
    const saving = ref(false)
    /** The entry being replaced, or `null` while the form is adding a new one. */
    const editing = ref<string | null>(null)
    const draft = ref<DictionaryDraft>(blankDraft())

    /** The next free attribute number, so adding an entry starts somewhere sensible. */
    const nextField = computed(() => {
        const used = entries.value.map((entry) => entry.field ?? 0)
        return String(Math.max(0, ...used) + 1)
    })

    function reset(): void {
        editing.value = null
        draft.value = {...blankDraft(), field: nextField.value}
    }

    async function load(): Promise<void> {
        const [selectedAccount, selectedFile] = [account.value, file.value]
        if (!selectedAccount || !selectedFile) {
            entries.value = []
            loaded.value = false
            return
        }
        try {
            entries.value = await accountsApi.dictionary(selectedAccount, selectedFile)
            alerts.clear()
        } catch (cause) {
            entries.value = []
            alerts.fail(cause)
        } finally {
            loaded.value = true
        }
    }

    /** Loads an existing entry into the form, to be saved back over itself. */
    function edit(entry: DictionaryEntry): void {
        editing.value = entry.name
        draft.value = {
            name: entry.name,
            field: entry.field === null ? '' : String(entry.field),
            heading: entry.heading,
            justification: entry.justification || 'L',
            width: entry.width === null ? '' : String(entry.width),
            association: entry.association,
            associationDepth: entry.associationDepth,
            conversion: entry.conversion,
        }
    }

    /**
     * Runs one change and re-reads the dictionary afterwards rather than
     * assuming what landed: `SET.DICT` fills in a default for whatever the form
     * left blank, and the stored entry is the one worth showing.
     *
     * The reload reports its own success, so a refusal is held and raised after
     * it - otherwise the banner explaining the refusal would be cleared by the
     * very reload that shows the dictionary unchanged.
     */
    async function change(action: () => Promise<unknown>): Promise<boolean> {
        const [selectedAccount, selectedFile] = [account.value, file.value]
        if (!selectedAccount || !selectedFile || saving.value) return false
        saving.value = true
        let failure: unknown = null
        try {
            await action()
        } catch (cause) {
            failure = cause
        }
        try {
            await load()
        } finally {
            saving.value = false
        }
        if (failure) {
            alerts.fail(failure)
            return false
        }
        return true
    }

    /** Stores the form's entry, emptying the form once the database has it. */
    async function save(): Promise<boolean> {
        const [selectedAccount, selectedFile] = [account.value, file.value]
        if (!selectedAccount || !selectedFile) return false
        const draftToSave = {...draft.value}
        const done = await change(() =>
            accountsApi.saveDictionaryEntry(selectedAccount, selectedFile, draftToSave),
        )
        if (done) reset()
        return done
    }

    async function remove(name: string): Promise<boolean> {
        const [selectedAccount, selectedFile] = [account.value, file.value]
        if (!selectedAccount || !selectedFile) return false
        const done = await change(() =>
            accountsApi.deleteDictionaryEntry(selectedAccount, selectedFile, name),
        )
        // An open edit of an entry that has just gone would save it back.
        if (editing.value === name) reset()
        return done
    }

    // A different file means a different dictionary, so whatever was half typed
    // about the old one is abandoned rather than applied to the new one. The
    // form is reset after the load so its suggested attribute number is the
    // next free one in the dictionary that is actually on screen.
    watch(
        [account, file],
        () => {
            loaded.value = false
            entries.value = []
            editing.value = null
            draft.value = blankDraft()
            void load().then(reset)
        },
        {immediate: true},
    )

    return {entries, loaded, saving, editing, draft, load, edit, reset, save, remove}
}
