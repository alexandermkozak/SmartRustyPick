/** The accounts slice's public surface. */

import type {FeatureTab} from '../types'
import AccountsView from './AccountsView.vue'

export const accountsTab: FeatureTab = {
    id: 'accounts',
    label: 'Accounts',
    component: AccountsView,
}
