/** The certificates slice's public surface. */

import type {FeatureTab} from '../types'
import CertificatesView from './CertificatesView.vue'

export const certificatesTab: FeatureTab = {
    id: 'certificates',
    label: 'Certificates',
    component: CertificatesView,
}
