/**
 * The authorization list and the actions that change it.
 *
 * The view renders; this decides. Keeping the reload-after-a-change rule in one
 * place means every action reports failure the same way and leaves the list
 * showing what the server actually holds.
 */

import {usePolling} from '@shared/composables/usePolling'
import {useAlerts} from '@shared/composables/useAlerts'
import {authorizationsApi} from '../api'
import type {AuthorizationRequest, ClientEntry} from '../types'

/** Splits a comma-separated account list from a form field. */
export function splitAccounts(value: string): string[] {
    return value
        .split(',')
        .map((account) => account.trim())
        .filter(Boolean)
}

export function useClients() {
    // The list changes only when someone changes it - here, in a CLI beside the
    // server, or in another dashboard - so it is polled slowly rather than not
    // at all.
    const clients = usePolling<ClientEntry[]>(authorizationsApi.list, {intervalMs: 15000})
    const alerts = useAlerts()

    /** Runs a change, then reloads so the table shows the server's version. */
    async function apply(action: () => Promise<unknown>): Promise<boolean> {
        const ok = await alerts.attempt(action)
        if (ok) await clients.refresh()
        return ok
    }

    return {
        clients,
        authorize: (request: AuthorizationRequest) =>
            apply(() => authorizationsApi.authorize(request)),
        changeAccounts: (name: string, accounts: string[], remove: boolean) =>
            apply(() => authorizationsApi.changeAccounts(name, accounts, remove)),
        revoke: (name: string) => apply(() => authorizationsApi.revoke(name)),
    }
}
