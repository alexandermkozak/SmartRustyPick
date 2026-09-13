/**
 * Taking a backup and putting one back, from a browser.
 *
 * An archive never touches the dashboard's own disk: it is downloaded straight
 * out of the response and uploaded straight from the file input, so the only
 * copy is the one the operator chose to keep. Writing an archive to a path on
 * the server host is the CLI's job - a path typed into a web form names a
 * directory on a machine the person at the keyboard usually cannot see.
 */

import {ref} from 'vue'
import {saveBlob} from '@shared/api/client'
import {useAlerts} from '@shared/composables/useAlerts'
import {archivesApi} from '../api'
import type {ExportScope, ImportOptions, ImportReport} from '../types'

export function useArchives() {
    const alerts = useAlerts()
    const accounts = ref<string[]>([])
    const exporting = ref(false)
    const importing = ref(false)
    const report = ref<ImportReport | null>(null)

    async function loadAccounts(): Promise<void> {
        try {
            accounts.value = await archivesApi.accounts()
        } catch (cause) {
            alerts.fail(cause)
        }
    }

    async function exportArchive(scope: ExportScope): Promise<boolean> {
        exporting.value = true
        try {
            const {blob, filename} = await archivesApi.export(scope)
            saveBlob(filename, blob)
            alerts.clear()
            return true
        } catch (cause) {
            alerts.fail(cause)
            return false
        } finally {
            exporting.value = false
        }
    }

    async function importArchive(archive: File, options: ImportOptions): Promise<boolean> {
        importing.value = true
        try {
            report.value = await archivesApi.import(archive, options)
            alerts.clear()
            return true
        } catch (cause) {
            // The previous report is cleared rather than left on screen: a
            // stale "restored 18,402 records" above a failure reads as though
            // the failure came after the restore.
            report.value = null
            alerts.fail(cause)
            return false
        } finally {
            importing.value = false
        }
    }

    return {accounts, exporting, importing, report, loadAccounts, exportArchive, importArchive}
}
