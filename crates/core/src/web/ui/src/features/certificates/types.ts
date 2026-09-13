/** A freshly issued certificate, returned once and never stored by the page. */
export interface GeneratedCert {
    common_name: string
    thumbprint: string
    certificate_pem: string
    private_key_pem: string
    ca_pem: string
    cert_path: string
    key_path: string
    pfx_path: string | null
    /**
     * The PKCS#12 import passphrase, returned once and stored nowhere. Present
     * exactly when `pfx_path` is.
     */
    pfx_passphrase: string | null
    /** RFC 3339 UTC, read from the certificate itself. */
    expires_at: string | null
}

/** The fields `GENERATE.CERT` needs. */
export interface CertificateRequest {
    common_name: string
    accounts: string[]
    is_admin: boolean
    capabilities: string[]
    /** Omitted to take the server's default; refused if above its ceiling. */
    days?: number
}
