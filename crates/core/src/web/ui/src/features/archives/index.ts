/** The archives slice's public surface. */

import type {FeatureTab} from '../types'
import ArchivesView from './ArchivesView.vue'

export const archivesTab: FeatureTab = {
    id: 'archives',
    label: 'Backup',
    component: ArchivesView,
}
