/**
 * The accounts slice, tested through its view.
 *
 * A slice owning its own tests is the other half of owning its own code: this
 * file moves, or is deleted, with the feature it covers.
 */

import {afterEach, beforeEach, describe, expect, it, vi} from 'vitest'
import {flushPromises, mount} from '@vue/test-utils'
import type {Component} from 'vue'
import type {AccountStats, FileStats} from './types'

const account: AccountStats = {
    name: 'SALES',
    directory: 'db_storage/SALES',
    file_count: 2,
    record_count: 1280,
    disk_bytes: 262144,
}

const fileStats: FileStats = {
    account: 'SALES',
    name: 'USERS',
    record_count: 1280,
    dict_count: 4,
    modulus: 128,
    version: 42,
    group_count: 128,
    smallest_group_bytes: 96,
    largest_group_bytes: 512,
    disk_bytes: 262144,
    checksums: true,
    legacy: false,
    durable: false,
    loaded: true,
    modified_seconds_ago: 12,
}

const envelope = (body: Record<string, unknown>) => ({
    status: 'OK',
    message: null,
    record: null,
    results: null,
    keys: null,
    count: null,
    ...body,
})

function stubFetch(routes: Record<string, unknown>) {
    return vi.fn(async (input: RequestInfo | URL) => {
        const path = String(input).split('?')[0]
        const body = routes[path]
        if (body === undefined) {
            return new Response(JSON.stringify({error: 'No such endpoint'}), {status: 404})
        }
        return new Response(JSON.stringify(body), {status: 200})
    })
}

const fileList = (durable: boolean) =>
    envelope({
        keys: ['DIR', 'USERS'],
        results: [
            ['DIR', {durable: false}],
            ['USERS', {durable}],
        ],
        count: 2,
    })

const routes = {
    '/api/accounts': envelope({results: [['SALES', account]], count: 1}),
    '/api/accounts/SALES/files': fileList(false),
    '/api/accounts/SALES/files/USERS': envelope({record: fileStats}),
}

/** Selects SALES, then USERS: the state every file-level test starts from. */
async function openUsers(View: Component) {
    const wrapper = mount(View)
    await flushPromises()
    await wrapper.findAll('.list button')[0].trigger('click')
    await flushPromises()
    await wrapper.findAll('.list')[1].findAll('button')[1].trigger('click')
    await flushPromises()
    return wrapper
}

describe('the accounts view', () => {
    let View: Component
    // The banner is App.vue's to render, so a refusal is asserted on the shared
    // state itself - taken from the same module instance the view just imported.
    let alerts: {message: {value: string | null}}

    beforeEach(async () => {
        vi.resetModules()
        vi.useFakeTimers()
        vi.stubGlobal('fetch', stubFetch(routes))
        View = (await import('./AccountsView.vue')).default
        alerts = (await import('@shared/composables/useAlerts')).useAlerts()
    })

    afterEach(() => {
        vi.useRealTimers()
        vi.unstubAllGlobals()
        vi.restoreAllMocks()
    })

    it('lists accounts with the figures that say how big they are', async () => {
        const wrapper = mount(View)
        await flushPromises()

        expect(wrapper.text()).toContain('SALES')
        expect(wrapper.text()).toContain('2 files')
        expect(wrapper.text()).toContain('1,280 records')
        expect(wrapper.text()).toContain('db_storage/SALES')
    })

    it("drills from an account to its files to one file's statistics", async () => {
        const wrapper = mount(View)
        await flushPromises()

        expect(wrapper.text()).toContain('Select an account.')
        await wrapper.findAll('.list button')[0].trigger('click')
        await flushPromises()

        expect(wrapper.text()).toContain('Files in SALES')
        expect(wrapper.text()).toContain('USERS')
        expect(wrapper.text()).toContain('Select a file.')

        const fileButtons = wrapper.findAll('.list')[1].findAll('button')
        await fileButtons[1].trigger('click')
        await flushPromises()

        expect(wrapper.text()).toContain('SALES/USERS')
        expect(wrapper.text()).toContain('Hash modulus')
        expect(wrapper.text()).toContain('128')
    })

    it('marks the durable files in the listing', async () => {
        vi.stubGlobal('fetch', stubFetch({...routes, '/api/accounts/SALES/files': fileList(true)}))
        const wrapper = mount(View)
        await flushPromises()
        await wrapper.findAll('.list button')[0].trigger('click')
        await flushPromises()

        const entries = wrapper.findAll('.list')[1].findAll('li')
        expect(entries[0].text()).not.toContain('durable')
        expect(entries[1].text()).toContain('durable')
    })

    it('promotes a file to durable writes and shows what the database then says', async () => {
        // The button reports the server's answer, not the click: the flag is the
        // database's to decide, and a global durable_writes overrules the request.
        const posted: Array<[string, string | undefined]> = []
        const fetchSpy = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
            const path = String(input).split('?')[0]
            if (init?.method === 'POST') {
                posted.push([path, init.body as string])
                return new Response(JSON.stringify(envelope({})), {status: 200})
            }
            const promoted = posted.length > 0
            const body =
                path === '/api/accounts/SALES/files'
                    ? fileList(promoted)
                    : path === '/api/accounts/SALES/files/USERS'
                      ? envelope({record: {...fileStats, durable: promoted}})
                      : routes[path as keyof typeof routes]
            if (body === undefined) {
                return new Response(JSON.stringify({error: 'No such endpoint'}), {status: 404})
            }
            return new Response(JSON.stringify(body), {status: 200})
        })
        vi.stubGlobal('fetch', fetchSpy)

        const wrapper = await openUsers(View)
        expect(wrapper.text()).toContain('Make durable')

        await wrapper.find('.file-actions button').trigger('click')
        await flushPromises()

        expect(posted).toEqual([['/api/accounts/SALES/files/USERS', '{"durable":true}']])
        expect(wrapper.find('.file-actions button').text()).toBe('Buffer writes')
        expect(wrapper.findAll('.list')[1].findAll('li')[1].text()).toContain('durable')
    })

    it('reports a refusal instead of pretending the flag changed', async () => {
        vi.stubGlobal(
            'fetch',
            vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
                if (init?.method === 'POST') {
                    return new Response(JSON.stringify({error: 'Admin privileges required'}), {
                        status: 403,
                    })
                }
                const path = String(input).split('?')[0]
                const body = routes[path as keyof typeof routes]
                if (body === undefined) {
                    return new Response(JSON.stringify({error: 'No such endpoint'}), {status: 404})
                }
                return new Response(JSON.stringify(body), {status: 200})
            }),
        )

        const wrapper = await openUsers(View)
        await wrapper.find('.file-actions button').trigger('click')
        await flushPromises()

        expect(wrapper.find('.file-actions button').text()).toBe('Make durable')
        expect(alerts.message.value).toBe('Admin privileges required')
    })

    it('offers no durability switch for DIR, which carries the flags', async () => {
        vi.stubGlobal(
            'fetch',
            stubFetch({
                ...routes,
                '/api/accounts/SALES/files/DIR': envelope({
                    record: {...fileStats, name: 'DIR', durable: false},
                }),
            }),
        )
        const wrapper = mount(View)
        await flushPromises()
        await wrapper.findAll('.list button')[0].trigger('click')
        await flushPromises()
        await wrapper.findAll('.list')[1].findAll('button')[0].trigger('click')
        await flushPromises()

        expect(wrapper.text()).toContain('SALES/DIR')
        expect(wrapper.find('.file-actions').exists()).toBe(false)
        expect(wrapper.text()).toContain('DIR carries the durability flags')
    })

    it('clears the selection when the account disappears from under it', async () => {
        // Someone else dropping an account must not leave the file list and the
        // statistics panel showing a directory that no longer exists.
        const wrapper = mount(View)
        await flushPromises()
        await wrapper.findAll('.list button')[0].trigger('click')
        await flushPromises()
        expect(wrapper.text()).toContain('Files in SALES')

        // The next poll finds the account gone. Fake timers have to be in place
        // before the poller schedules its next tick, so they are installed for
        // the whole test rather than switched on halfway through.
        vi.stubGlobal(
            'fetch',
            stubFetch({...routes, '/api/accounts': envelope({results: [], count: 0})}),
        )
        await vi.advanceTimersByTimeAsync(20_000)
        await flushPromises()

        expect(wrapper.text()).toContain('No accounts.')
        expect(wrapper.text()).toContain('Select an account.')
    })
})
