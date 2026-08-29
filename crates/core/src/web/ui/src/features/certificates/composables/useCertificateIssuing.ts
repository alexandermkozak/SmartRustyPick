/**
 * Issuing a certificate, and the files that come back with it.
 *
 * The private key is returned once and deliberately not stored anywhere, so
 * this holds it only for as long as the view showing it is open.
 */

import {computed, ref} from 'vue'
import {useAlerts} from '@shared/composables/useAlerts'
import {certificatesApi} from '../api'
import type {CertificateRequest, GeneratedCert} from '../types'

export interface DownloadableFile {
    label: string
    filename: string
    contents: string
}

export function useCertificateIssuing() {
    const alerts = useAlerts()
    const issuing = ref(false)
    const issued = ref<GeneratedCert | null>(null)

    /** The three files a client needs, in the order it needs them. */
    const files = computed<DownloadableFile[]>(() => {
        const cert = issued.value
        if (!cert) return []
        return [
            {
                label: 'certificate',
                filename: `${cert.common_name}.crt`,
                contents: cert.certificate_pem,
            },
            {
                label: 'private key',
                filename: `${cert.common_name}.key`,
                contents: cert.private_key_pem,
            },
            {label: 'CA certificate', filename: 'ca.crt', contents: cert.ca_pem},
        ]
    })

    async function issue(request: CertificateRequest): Promise<boolean> {
        issuing.value = true
        try {
            issued.value = await certificatesApi.issue(request)
            alerts.clear()
            return true
        } catch (cause) {
            alerts.fail(cause)
            return false
        } finally {
            issuing.value = false
        }
    }

    return {issuing, issued, files, issue}
}

/**
 * Hands the viewer a file the page holds in memory.
 *
 * The material never touches the server's filesystem on the way to the browser
 * and is not stored here either, so a download is the only way to keep it.
 */
export function download(filename: string, contents: string): void {
    const url = URL.createObjectURL(new Blob([contents], {type: 'application/x-pem-file'}))
    const link = document.createElement('a')
    link.href = url
    link.download = filename
    document.body.appendChild(link)
    link.click()
    link.remove()
    window.setTimeout(() => URL.revokeObjectURL(url), 10_000)
}
