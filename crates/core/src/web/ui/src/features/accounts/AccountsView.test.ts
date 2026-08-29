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

const routes = {
    '/api/accounts': envelope({results: [['SALES', account]], count: 1}),
    '/api/accounts/SALES/files': envelope({keys: ['DIR', 'USERS'], count: 2}),
    '/api/accounts/SALES/files/USERS': envelope({record: fileStats}),
}

describe('the accounts view', () => {
    let View: Component

    beforeEach(async () => {
        vi.resetModules()
        vi.useFakeTimers()
        vi.stubGlobal('fetch', stubFetch(routes))
        View = (await import('./AccountsView.vue')).default
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
