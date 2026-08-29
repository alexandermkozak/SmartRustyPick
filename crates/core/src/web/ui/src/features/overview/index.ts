/**
 * The overview slice's public surface.
 *
 * Everything else in here is private to the slice: other features import from
 * this file or not at all, and the boundary test in
 * `shared/architecture.test.ts` enforces it.
 */

import type {FeatureTab} from '../types'
import OverviewView from './OverviewView.vue'

export {default as ServerLine} from './components/ServerLine.vue'
export {default as ServerControls} from './components/ServerControls.vue'

export const overviewTab: FeatureTab = {
    id: 'overview',
    label: 'Overview',
    component: OverviewView,
}
