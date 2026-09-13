/**
 * The capabilities a client can hold, and what each one is for.
 *
 * Shared rather than per-feature because both the authorization form and the
 * certificate form grant the same set, and a list that drifted between the two
 * would be a page offering a grant the server does not have.
 *
 * The wire names are the server's (`db::models::Capability`); the descriptions
 * exist because "clients:manage" does not tell an operator that it is the one
 * grant that can escalate into any other.
 */
export interface CapabilityInfo {
    /** The wire name, sent verbatim to the server. */
    name: string
    label: string
    detail: string
}

export const CAPABILITIES: readonly CapabilityInfo[] = [
    {
        name: 'accounts:manage',
        label: 'Manage accounts',
        detail: 'Create and drop accounts. Does not grant access to the records in them.',
    },
    {
        name: 'clients:manage',
        label: 'Manage clients',
        detail: 'Authorize and revoke clients, and issue certificates — so it can grant any other access.',
    },
    {
        name: 'server:observe',
        label: 'Observe the server',
        detail: 'Read server statistics and the authorized-client list. Reads no records.',
    },
]
