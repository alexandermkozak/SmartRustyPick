/**
 * The archives slice, tested through its view.
 *
 * What matters here is not that the panels render. It is that the *destructive*
 * options cannot be set by accident: a restore defaults to verifying, overwrite
 * is off unless asked for, and neither is sent as a query parameter the server
 * would read as consent.
 */

import {afterEach, beforeEach, describe, expect, it, vi} from 'vitest'
import {flushPromises, mount, type VueWrapper} from '@vue/test-utils'
import ArchivesView from './ArchivesView.vue'
import type {ImportReport} from './types'

const report: ImportReport = {
    source: 'account SALES',
    taken: 1757692800000,
    takenUtc: '2026-09-12 16:00:00 UTC',
    archiveFormat: 1,
    storageFormat: 1,
    dryRun: true,
    accountsCreated: [],
    files: [
        {
            account: 'SALES.COPY',
            file: 'ORDERS',
            action: 'created',
            records: 18402,
            dictionary: 6,
            bytes: 1904112,
        },
    ],
    records: 18402,
}

/** Every request the page made, so a test can assert the URL it asked for. */
let requests: Array<{url: string; init?: RequestInit}>

/**
 * The last request to the archive endpoint.
 *
 * Not simply the last request: a restore that actually wrote something reloads
 * the account list afterwards, because it may have created one. That reload is
 * correct and would otherwise be the entry every assertion here landed on.
 */
function lastArchiveRequest(): {url: string; init?: RequestInit} | undefined {
    return requests.filter((request) => request.url.startsWith('/api/archive')).at(-1)
}

function stubFetch(): void {
    requests = []
    vi.stubGlobal(
        'fetch',
        vi.fn(async (url: string, init?: RequestInit) => {
            requests.push({url, init})
            if (url.startsWith('/api/accounts')) {
                return new Response(
                    JSON.stringify({
                        status: 'OK',
                        results: [
                            ['SALES', {}],
                            ['STOCK', {}],
                        ],
                    }),
                    {status: 200},
                )
            }
            if (init?.method === 'POST') {
                return new Response(JSON.stringify({status: 'OK', archive: report}), {status: 200})
            }
            return new Response(new Blob(['SRPARC01']), {
                status: 200,
                headers: {'Content-Disposition': 'attachment; filename="SALES-20260912-1600.srp"'},
            })
        }),
    )
}

/** jsdom has no object URLs and no real downloads; the page only needs them not to throw. */
function stubDownloads(): void {
    vi.stubGlobal('URL', {
        ...URL,
        createObjectURL: vi.fn(() => 'blob:stub'),
        revokeObjectURL: vi.fn(),
    })
    HTMLAnchorElement.prototype.click = vi.fn()
}

async function open(): Promise<VueWrapper> {
    const wrapper = mount(ArchivesView)
    await flushPromises()
    return wrapper
}

/** Puts a file on the input the way a chooser does, since jsdom will not. */
async function choose(wrapper: VueWrapper, name = 'backup.srp'): Promise<void> {
    const input = wrapper.find('input[type="file"]')
    const file = new File(['SRPARC01'], name)
    Object.defineProperty(input.element, 'files', {value: [file], configurable: true})
    await input.trigger('change')
}

beforeEach(() => {
    stubFetch()
    stubDownloads()
})

afterEach(() => vi.unstubAllGlobals())

describe('exporting', () => {
    it('offers every account and defaults to all of them', async () => {
        const wrapper = await open()
        const options = wrapper.findAll('select option').map((option) => option.text())
        expect(options).toEqual(['Every account', 'SALES', 'STOCK'])
        expect((wrapper.find('select').element as HTMLSelectElement).value).toBe('')
    })

    it('asks for the whole database when no account is chosen', async () => {
        const wrapper = await open()
        await wrapper.find('form').trigger('submit')
        await flushPromises()
        // No scope in the query at all, which is how EXPORT.BYTES is told
        // "everything" - rather than a magic account name.
        expect(lastArchiveRequest()?.url).toBe('/api/archive')
    })

    it('names the account and the file it was given', async () => {
        const wrapper = await open()
        await wrapper.find('select').setValue('SALES')
        await wrapper.find('form input[type="text"], form input:not([type])').setValue('ORDERS')
        await wrapper.find('form').trigger('submit')
        await flushPromises()
        expect(lastArchiveRequest()?.url).toBe('/api/archive?account=SALES&file=ORDERS')
    })

    it('will not let a file be named without the account it belongs to', async () => {
        const wrapper = await open()
        await wrapper.find('select').setValue('SALES')
        const file = wrapper.find('form input:not([type=file])')
        await file.setValue('ORDERS')
        // Going back to "every account" clears the file rather than leaving a
        // scope the server would have to refuse.
        await wrapper.find('select').setValue('')
        await flushPromises()
        expect((file.element as HTMLInputElement).value).toBe('')
    })
})

describe('restoring', () => {
    it('verifies by default and asks for neither overwrite nor a rename', async () => {
        const wrapper = await open()
        await choose(wrapper)
        await wrapper.findAll('form')[1].trigger('submit')
        await flushPromises()

        const last = lastArchiveRequest()
        expect(last?.init?.method).toBe('POST')
        // The property worth pinning: a submit nobody configured is a dry run,
        // and `overwrite` is absent rather than false - the server reads an
        // absent flag as off, and sending one at all is a decision.
        expect(last?.url).toBe('/api/archive?verify=true')
    })

    it('sends overwrite only when it was actually ticked', async () => {
        const wrapper = await open()
        await choose(wrapper)
        const checks = wrapper.findAll('input[type="checkbox"]')
        await checks[0].setValue(false) // verify off
        await checks[1].setValue(true) // overwrite on
        await wrapper.findAll('form')[1].trigger('submit')
        await flushPromises()
        expect(lastArchiveRequest()?.url).toBe('/api/archive?overwrite=true')
    })

    it('carries the account to restore into', async () => {
        const wrapper = await open()
        await choose(wrapper)
        await wrapper.findAll('form')[1].find('input:not([type=file])').setValue('SALES.COPY')
        await wrapper.findAll('form')[1].trigger('submit')
        await flushPromises()
        expect(lastArchiveRequest()?.url).toBe('/api/archive?into=SALES.COPY&verify=true')
    })

    it('cannot be submitted before an archive is chosen', async () => {
        const wrapper = await open()
        const button = wrapper.findAll('form')[1].find('button')
        expect(button.attributes('disabled')).toBeDefined()
    })

    it('reports what the restore did, file by file', async () => {
        const wrapper = await open()
        await choose(wrapper)
        await wrapper.findAll('form')[1].trigger('submit')
        await flushPromises()

        const text = wrapper.text()
        expect(text).toContain('Verified')
        expect(text).toContain('account SALES')
        expect(text).toContain('2026-09-12 16:00:00 UTC')
        expect(text).toContain('SALES.COPY')
        expect(text).toContain('Nothing was written.')
    })

    it('clears a previous report when a later attempt fails', async () => {
        const wrapper = await open()
        await choose(wrapper)
        await wrapper.findAll('form')[1].trigger('submit')
        await flushPromises()
        expect(wrapper.text()).toContain('Verified')

        vi.stubGlobal(
            'fetch',
            vi.fn(
                async () =>
                    new Response(JSON.stringify({error: 'Not a usable archive'}), {status: 400}),
            ),
        )
        await wrapper.findAll('form')[1].trigger('submit')
        await flushPromises()

        // A stale success above a failure reads as though the failure came
        // after the restore.
        expect(wrapper.text()).not.toContain('Verified')
    })
})
