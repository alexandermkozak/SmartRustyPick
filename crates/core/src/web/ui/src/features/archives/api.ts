import {encode, fetchBytes, pairs, sendBytes} from '@shared/api/client'
import type {ProtocolResponse} from '@shared/api/protocol'
import type {ExportScope, ImportOptions, ImportReport} from './types'

/** The query an export's scope becomes: a file, an account, or neither. */
function scopeQuery({account, file}: ExportScope): string {
    const parts: string[] = []
    if (account) parts.push(`account=${encode(account)}`)
    if (file) parts.push(`file=${encode(file)}`)
    return parts.length ? `?${parts.join('&')}` : ''
}

/** The query a restore's options become. Absent means off, never "unchanged". */
function optionsQuery({into, overwrite, verify}: ImportOptions): string {
    const parts: string[] = []
    if (into) parts.push(`into=${encode(into)}`)
    if (overwrite) parts.push('overwrite=true')
    if (verify) parts.push('verify=true')
    return parts.length ? `?${parts.join('&')}` : ''
}

export const archivesApi = {
    /**
     * `LIST.ACCOUNTS`, for the picker.
     *
     * Asked for here rather than borrowed from the accounts slice: a feature
     * owns its own calls, and one importing another's API is exactly what
     * `shared/architecture.test.ts` forbids. The cost is one small duplicated
     * request; the alternative is two slices that cannot be moved apart.
     */
    accounts: (): Promise<string[]> =>
        pairs<unknown>('/api/accounts').then((entries) => entries.map(([name]) => name)),

    /** `EXPORT.BYTES`: the archive as a download, with the name the server chose. */
    export: (scope: ExportScope): Promise<{blob: Blob; filename: string}> =>
        fetchBytes(`/api/archive${scopeQuery(scope)}`),

    /** `IMPORT.BYTES`: the uploaded archive, restored or merely reported on. */
    import: (archive: Blob, options: ImportOptions): Promise<ImportReport> =>
        sendBytes<ProtocolResponse<never> & {archive?: ImportReport}>(
            `/api/archive${optionsQuery(options)}`,
            archive,
        ).then((response) => {
            if (!response.archive) throw new Error('The database returned no report')
            return response.archive
        }),
}
