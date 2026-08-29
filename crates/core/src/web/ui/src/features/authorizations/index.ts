/** The authorizations slice's public surface. */

import type {FeatureTab} from '../types'
import AuthorizationsView from './AuthorizationsView.vue'

export const authorizationsTab: FeatureTab = {
    id: 'authorizations',
    label: 'Authorizations',
    component: AuthorizationsView,
}
