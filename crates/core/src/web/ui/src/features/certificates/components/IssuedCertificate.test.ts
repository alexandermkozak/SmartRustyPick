/**
 * The passphrase block, which exists because the server keeps no copy of what
 * it shows. If this render is wrong, the bundle it protects is unusable and
 * nothing else in the system can say so.
 */

import {describe, expect, it} from 'vitest'
import {mount} from '@vue/test-utils'
import IssuedCertificate from './IssuedCertificate.vue'
import type {GeneratedCert} from '../types'

const issued: GeneratedCert = {
    common_name: 'reporting-bot',
    thumbprint: '9f86d081',
    certificate_pem: '-----BEGIN CERTIFICATE-----\nleaf\n',
    private_key_pem: '-----BEGIN PRIVATE KEY-----\nkey\n',
    ca_pem: '-----BEGIN CERTIFICATE-----\nca\n',
    cert_path: '.local/certs/reporting-bot.crt',
    key_path: '.local/certs/reporting-bot.key',
    pfx_path: '.local/certs/reporting-bot.pfx',
    pfx_passphrase: 'a3f1c08e57d2b9416ef0',
    expires_at: '2027-09-13T16:23:45Z',
}

const files = [
    {label: 'certificate', filename: 'reporting-bot.crt', contents: issued.certificate_pem},
]

describe('the issued certificate card', () => {
    it('shows the bundle passphrase, because this render is the only copy', () => {
        const wrapper = mount(IssuedCertificate, {props: {certificate: issued, files}})

        expect(wrapper.find('.passphrase').exists()).toBe(true)
        expect(wrapper.find('.passphrase code').text()).toBe('a3f1c08e57d2b9416ef0')
        expect(wrapper.text()).toContain('The server keeps no copy')
    })

    it('says nothing about a passphrase when there is no bundle', () => {
        const wrapper = mount(IssuedCertificate, {
            props: {
                certificate: {...issued, pfx_path: null, pfx_passphrase: null},
                files,
            },
        })

        expect(wrapper.find('.passphrase').exists()).toBe(false)
        expect(wrapper.text()).toContain('not generated')
    })
})
