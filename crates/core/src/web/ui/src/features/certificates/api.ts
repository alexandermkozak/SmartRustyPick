import {record} from '@shared/api/client'
import type {CertificateRequest, GeneratedCert} from './types'

export const certificatesApi = {
    /** `GENERATE.CERT`: issues, authorizes and returns the material in one step. */
    issue: (request: CertificateRequest): Promise<GeneratedCert> =>
        record<GeneratedCert>('/api/certificates', {method: 'POST', body: JSON.stringify(request)}),
}
