import {call, encode, pairs} from '@shared/api/client'
import type {AuthorizationRequest, ClientEntry, ClientInfo} from './types'

export const authorizationsApi = {
    /** `LIST.CONNS`: every client the server will accept a certificate from. */
    async list(): Promise<ClientEntry[]> {
        const results = await pairs<ClientInfo>('/api/clients')
        return results.map(([name, info]) => ({name, info}))
    },

    /** `AUTHORIZE.CONN` for a certificate that already exists. */
    authorize: (client: AuthorizationRequest): Promise<unknown> =>
        call('/api/clients', {method: 'POST', body: JSON.stringify(client)}),

    /** `DEAUTHORIZE.CONN`. The certificate stops working immediately. */
    revoke: (name: string): Promise<unknown> =>
        call(`/api/clients/${encode(name)}`, {method: 'DELETE'}),

    /** `ADD.CLIENT.ACCOUNT` or `REMOVE.CLIENT.ACCOUNT`. */
    changeAccounts: (name: string, accounts: string[], remove: boolean): Promise<unknown> =>
        call(`/api/clients/${encode(name)}/accounts`, {
            method: 'POST',
            body: JSON.stringify({accounts, remove}),
        }),
}
