/**
 * Smoke tests for the dashboard shell.
 *
 * The Rust side proves the server answers; these prove the page does something
 * sensible with the answers - that it mounts, polls, renders real values, and
 * survives the API failing. `fetch` is stubbed with the shapes the endpoints
 * really return, so a field renamed in Rust breaks the types in `types.ts` and
 * a field mis-read here breaks these.
 */

import {afterEach, beforeEach, describe, expect, it, vi} from 'vitest'
import {flushPromises, mount} from '@vue/test-utils'
import type {Component} from 'vue'
import type {ServerSnapshot} from '@features/overview/types'

const snapshot: ServerSnapshot = {
    uptime_seconds: 3661,
    started_at: 1_756_400_000,
    listen_addr: '127.0.0.1:8443',
    total_connections: 12,
    rejected_connections: 1,
    total_requests: 340,
    failed_requests: 2,
    pending_writes: 0,
    loaded_tables: 3,
    authorized_clients: 2,
    active_connections: [
        {
            id: 12,
            peer: '127.0.0.1:52344',
            client_name: 'WEB.DASHBOARD',
            thumbprint: 'abc123',
            is_admin: true,
            connected_seconds: 300,
            requests: 40,
            last_command: 'SERVER.STATS',
            idle_seconds: 0,
        },
    ],
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

/** Answers the endpoints the way the dashboard's HTTP API does. */
function stubFetch(routes: Record<string, unknown>, status = 200) {
    return vi.fn(async (input: RequestInfo | URL) => {
        const path = String(input).split('?')[0]
        const body = routes[path]
        if (body === undefined) {
            return new Response(JSON.stringify({error: 'No such endpoint'}), {status: 404})
        }
        return new Response(JSON.stringify(body), {
            status,
            headers: {'Content-Type': 'application/json'},
        })
    })
}

describe('the dashboard shell', () => {
    // The server-stats poller is a module-scope singleton, deliberately: one poll
    // feeds the header and the overview for the life of the page. Each test gets
    // a fresh module graph so it starts from the state a freshly loaded page has.
    let App: Component

    beforeEach(async () => {
        vi.resetModules()
        vi.stubGlobal('fetch', stubFetch({'/api/stats': envelope({record: snapshot})}))
        App = (await import('./App.vue')).default
    })

    afterEach(() => {
        vi.unstubAllGlobals()
        vi.restoreAllMocks()
    })

    it('offers every management view', () => {
        const wrapper = mount(App)
        const tabs = wrapper.findAll('.tab').map((tab) => tab.text())
        expect(tabs).toEqual(['Overview', 'Authorizations', 'Certificates', 'Accounts'])
    })

    it('shows the server it is connected to once the first poll lands', async () => {
        const wrapper = mount(App)
        expect(wrapper.find('.sub').text()).toBe('connecting…')

        await flushPromises()

        expect(wrapper.find('.sub').text()).toBe('127.0.0.1:8443 · up 1h 1m')
        expect(wrapper.find('.pill').text()).toBe('connected')
    })

    it('renders the live figures from the snapshot', async () => {
        const wrapper = mount(App)
        await flushPromises()

        const cards = wrapper.findAll('.stat').map((card) => card.text())
        expect(cards.some((text) => text.includes('340') && text.includes('Requests'))).toBe(true)
        expect(
            cards.some((text) => text.includes('Active connections') && text.includes('1')),
        ).toBe(true)

        // The open session is listed with the name it was authorized under.
        expect(wrapper.text()).toContain('WEB.DASHBOARD')
        expect(wrapper.text()).toContain('SERVER.STATS')
    })

    it('switches views without losing the header', async () => {
        const wrapper = mount(App)
        await flushPromises()

        await wrapper.findAll('.tab')[2].trigger('click')
        await flushPromises()

        expect(wrapper.text()).toContain('Issue a client certificate')
        expect(wrapper.find('.sub').text()).toContain('127.0.0.1:8443')
    })

    it('reports a refused session instead of silently showing nothing', async () => {
        vi.stubGlobal(
            'fetch',
            vi.fn(
                async () =>
                    new Response(JSON.stringify({error: 'A valid dashboard token is required'}), {
                        status: 401,
                    }),
            ),
        )

        const wrapper = mount(App)
        await flushPromises()

        expect(wrapper.find('.pill').text()).toBe('disconnected')
        expect(wrapper.text()).toContain('A valid dashboard token is required')
    })

    it('stops polling once the session is refused, rather than retrying forever', async () => {
        const fetchStub = vi.fn(
            async () =>
                new Response(JSON.stringify({error: 'A valid dashboard token is required'}), {
                    status: 401,
                }),
        )
        vi.stubGlobal('fetch', fetchStub)
        vi.useFakeTimers()

        mount(App)
        await flushPromises()
        const afterFirstFailure = fetchStub.mock.calls.length

        await vi.advanceTimersByTimeAsync(30_000)
        expect(fetchStub.mock.calls.length).toBe(afterFirstFailure)

        vi.useRealTimers()
    })
})
