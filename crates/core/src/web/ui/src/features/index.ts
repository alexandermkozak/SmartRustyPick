/**
 * The feature registry: the one place that knows every slice.
 *
 * This is the composition point, and the only module allowed to import from
 * more than one feature - `shared/architecture.test.ts` enforces that. Adding a
 * feature means adding its directory and one line here.
 */

import {overviewTab} from './overview'
import {authorizationsTab} from './authorizations'
import {certificatesTab} from './certificates'
import {accountsTab} from './accounts'
import type {FeatureTab} from './types'

/** Tabs in the order they appear. */
export const featureTabs: FeatureTab[] = [
    overviewTab,
    authorizationsTab,
    certificatesTab,
    accountsTab,
]

export type {FeatureTab}
