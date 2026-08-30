/**
 * The accounts slice, tested through its view.
 *
 * A slice owning its own tests is the other half of owning its own code: this
 * file moves, or is deleted, with the feature it covers.
 */

import {afterEach, beforeEach, describe, expect, it, vi} from 'vitest'
import {flushPromises, mount, type VueWrapper} from '@vue/test-utils'
import type {Component} from 'vue'
import type {AccountStats, DictionaryEntry, FileStats} from './types'

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

const dictionary: DictionaryEntry[] = [
    {
        name: 'NAME',
        field: 1,
        heading: 'Name',
        justification: 'L',
        width: 20,
        conversion: '',
        definition: '1^Name^L^20',
    },
    {
        name: 'EMAIL',
        field: 2,
        heading: 'EMAIL',
        justification: 'L',
        width: 30,
        conversion: '',
        definition: '2^EMAIL^L^30',
    },
]

const dictionaryList = (entries: DictionaryEntry[] = dictionary) =>
    envelope({
        keys: entries.map((entry) => entry.name),
        results: entries.map(({name, ...rest}) => [name, rest]),
        count: entries.length,
    })

const routes = {
    '/api/accounts': envelope({results: [['SALES', account]], count: 1}),
    '/api/accounts/SALES/files': fileList(false),
    '/api/accounts/SALES/files/USERS': envelope({record: fileStats}),
    '/api/accounts/SALES/files/USERS/dictionary': dictionaryList(),
    '/api/accounts/SALES/files/DIR/dictionary': dictionaryList([]),
}

/** Every request the page made, in order, as `METHOD /path`. */
let traffic: string[] = []
/** Every request that changed something, with what it carried. */
let sent: Array<{method: string; path: string; body: string | undefined}> = []

function reset(): void {
    traffic = []
    sent = []
}

/**
 * A server that answers the listings and records what is written to it.
 *
 * `overrides` replaces a route's body, which is how a test says what the
 * database looks like after the change it has just made; `failures` refuses one
 * `METHOD /path` with a message, the way the protocol refuses a command.
 */
function stubServer(
    overrides: Record<string, unknown> = {},
    failures: Record<string, string> = {},
) {
    return vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        const path = String(input).split('?')[0]
        const method = init?.method ?? 'GET'
        traffic.push(`${method} ${path}`)
        if (method !== 'GET') {
            sent.push({method, path, body: init?.body as string | undefined})
            const refusal = failures[`${method} ${path}`]
            if (refusal) return new Response(JSON.stringify({error: refusal}), {status: 403})
            return new Response(JSON.stringify(envelope({})), {status: 200})
        }
        const body = {...routes, ...overrides}[path]
        if (body === undefined) {
            return new Response(JSON.stringify({error: 'No such endpoint'}), {status: 404})
        }
        return new Response(JSON.stringify(body), {status: 200})
    })
}

/**
 * The dictionary section. The view has two root elements, so a descendant
 * selector from the wrapper only reaches inside the first of them - queries for
 * the dictionary start from the section itself.
 */
const dict = (wrapper: VueWrapper) => wrapper.find('.dictionary')

/** The selection buttons of a list; the Drop beside each one is not one. */
const selectors = (wrapper: VueWrapper, list: number) =>
    wrapper.findAll('.list')[list].findAll('button.select')

/** Selects SALES, then USERS: the state every file-level test starts from. */
async function openUsers(View: Component) {
    const wrapper = mount(View)
    await flushPromises()
    await selectors(wrapper, 0)[0].trigger('click')
    await flushPromises()
    await selectors(wrapper, 1)[1].trigger('click')
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
        await selectors(wrapper, 0)[0].trigger('click')
        await flushPromises()

        expect(wrapper.text()).toContain('Files in SALES')
        expect(wrapper.text()).toContain('USERS')
        expect(wrapper.text()).toContain('Select a file.')

        await selectors(wrapper, 1)[1].trigger('click')
        await flushPromises()

        expect(wrapper.text()).toContain('SALES/USERS')
        expect(wrapper.text()).toContain('Hash modulus')
        expect(wrapper.text()).toContain('128')
    })

    it('marks the durable files in the listing', async () => {
        vi.stubGlobal('fetch', stubFetch({...routes, '/api/accounts/SALES/files': fileList(true)}))
        const wrapper = mount(View)
        await flushPromises()
        await selectors(wrapper, 0)[0].trigger('click')
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
        await selectors(wrapper, 0)[0].trigger('click')
        await flushPromises()
        await selectors(wrapper, 1)[0].trigger('click')
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
        await selectors(wrapper, 0)[0].trigger('click')
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

describe('account and file maintenance', () => {
    let View: Component
    let alerts: {message: {value: string | null}}

    beforeEach(async () => {
        vi.resetModules()
        reset()
        vi.stubGlobal('fetch', stubServer())
        vi.stubGlobal(
            'confirm',
            vi.fn(() => true),
        )
        View = (await import('./AccountsView.vue')).default
        alerts = (await import('@shared/composables/useAlerts')).useAlerts()
    })

    afterEach(() => {
        vi.unstubAllGlobals()
        vi.restoreAllMocks()
    })

    it('creates an account and re-reads the list rather than waiting for the poll', async () => {
        const wrapper = mount(View)
        await flushPromises()

        await wrapper.find('.new-account input').setValue('  REPORTS  ')
        await wrapper.find('.new-account').trigger('submit')
        await flushPromises()

        expect(sent).toEqual([{method: 'POST', path: '/api/accounts', body: '{"name":"REPORTS"}'}])
        // Re-read straight after, so a created account appears now rather than
        // whenever the twenty-second poll next comes round.
        expect(traffic).toEqual(['GET /api/accounts', 'POST /api/accounts', 'GET /api/accounts'])
    })

    it('confirms before dropping an account, and says what goes with it', async () => {
        const wrapper = mount(View)
        await flushPromises()

        await wrapper.findAll('.list button.danger')[0].trigger('click')
        await flushPromises()

        expect(window.confirm).toHaveBeenCalledWith(
            'Drop "SALES" and its 2 files? This cannot be undone.',
        )
        expect(sent).toEqual([{method: 'DELETE', path: '/api/accounts/SALES', body: undefined}])
    })

    it('leaves an account alone when the confirmation is declined', async () => {
        vi.stubGlobal(
            'confirm',
            vi.fn(() => false),
        )
        const wrapper = mount(View)
        await flushPromises()

        await wrapper.findAll('.list button.danger')[0].trigger('click')
        await flushPromises()

        expect(sent).toEqual([])
    })

    it('creates a file in the selected account, durable when asked', async () => {
        const wrapper = mount(View)
        await flushPromises()
        await selectors(wrapper, 0)[0].trigger('click')
        await flushPromises()

        await wrapper.find('.new-file input[type="checkbox"]').setValue(true)
        await wrapper.find('.new-file input:not([type="checkbox"])').setValue('LEDGER')
        await wrapper.find('.new-file').trigger('submit')
        await flushPromises()

        expect(sent).toEqual([
            {
                method: 'POST',
                path: '/api/accounts/SALES/files',
                body: '{"name":"LEDGER","durable":true}',
            },
        ])
    })

    it('drops a file and clears the statistics that described it', async () => {
        const wrapper = await openUsers(View)
        expect(wrapper.text()).toContain('SALES/USERS')

        vi.stubGlobal(
            'fetch',
            stubServer({
                '/api/accounts/SALES/files': envelope({
                    keys: ['DIR'],
                    results: [['DIR', {durable: false}]],
                    count: 1,
                }),
            }),
        )
        await wrapper.findAll('.list')[1].find('button.danger').trigger('click')
        await flushPromises()

        expect(sent).toEqual([
            {method: 'DELETE', path: '/api/accounts/SALES/files/USERS', body: undefined},
        ])
        expect(wrapper.text()).toContain('Select a file.')
        expect(wrapper.text()).not.toContain('SALES/USERS')
    })

    it("offers no Drop for DIR, which carries the account's file listing", async () => {
        const wrapper = mount(View)
        await flushPromises()
        await selectors(wrapper, 0)[0].trigger('click')
        await flushPromises()

        const entries = wrapper.findAll('.list')[1].findAll('li')
        expect(entries[0].text()).toContain('DIR')
        expect(entries[0].find('button.danger').exists()).toBe(false)
        expect(entries[1].find('button.danger').exists()).toBe(true)
    })

    it('reports a refusal instead of pretending the account was created', async () => {
        vi.stubGlobal('fetch', stubServer({}, {'POST /api/accounts': 'Admin privileges required'}))
        const wrapper = mount(View)
        await flushPromises()

        await wrapper.find('.new-account input').setValue('REPORTS')
        await wrapper.find('.new-account').trigger('submit')
        await flushPromises()

        expect(alerts.message.value).toBe('Admin privileges required')
    })
})

describe('the dictionary of a file', () => {
    let View: Component
    let alerts: {message: {value: string | null}}

    beforeEach(async () => {
        vi.resetModules()
        reset()
        vi.stubGlobal('fetch', stubServer())
        vi.stubGlobal(
            'confirm',
            vi.fn(() => true),
        )
        View = (await import('./AccountsView.vue')).default
        alerts = (await import('@shared/composables/useAlerts')).useAlerts()
    })

    afterEach(() => {
        vi.unstubAllGlobals()
        vi.restoreAllMocks()
    })

    /** The dictionary form's inputs, in the order the form declares them. */
    const fields = (wrapper: VueWrapper) => dict(wrapper).findAll('.form input')
    const rows = (wrapper: VueWrapper) => dict(wrapper).findAll('tbody tr')
    const submit = (wrapper: VueWrapper) => dict(wrapper).find('.form').trigger('submit')

    it('appears only once a file is selected, and shows what each field is', async () => {
        const wrapper = mount(View)
        await flushPromises()
        expect(wrapper.find('.dictionary').exists()).toBe(false)

        const opened = await openUsers(View)
        expect(opened.text()).toContain('Dictionary of SALES/USERS')
        const cells = rows(opened)[0].findAll('td')
        expect(cells[0].text()).toBe('NAME')
        expect(cells[1].text()).toBe('1')
        expect(cells[2].text()).toBe('Name')
        expect(cells[4].text()).toBe('20')
    })

    it('says so when a file has no dictionary at all', async () => {
        const wrapper = mount(View)
        await flushPromises()
        await selectors(wrapper, 0)[0].trigger('click')
        await flushPromises()
        await selectors(wrapper, 1)[0].trigger('click')
        await flushPromises()

        expect(wrapper.text()).toContain('SALES/DIR')
        expect(wrapper.text()).toContain('no dictionary entries')
    })

    it('adds an entry, suggesting the next free attribute number', async () => {
        const wrapper = await openUsers(View)

        // The dictionary uses attributes 1 and 2, so a new entry starts at 3.
        expect((fields(wrapper)[1].element as HTMLInputElement).value).toBe('3')

        await fields(wrapper)[0].setValue('PRICE')
        await dict(wrapper).find('.form select').setValue('R')
        await fields(wrapper)[3].setValue('12')
        await fields(wrapper)[4].setValue('MD2')
        await submit(wrapper)
        await flushPromises()

        expect(sent).toEqual([
            {
                method: 'POST',
                path: '/api/accounts/SALES/files/USERS/dictionary',
                body: JSON.stringify({
                    name: 'PRICE',
                    field: '3',
                    heading: '',
                    justification: 'R',
                    width: '12',
                    conversion: 'MD2',
                }),
            },
        ])
        // Emptied for the next entry, with the attribute number moved on.
        expect((fields(wrapper)[0].element as HTMLInputElement).value).toBe('')
    })

    it('loads an entry into the form to be edited, and saves it back under its name', async () => {
        const wrapper = await openUsers(View)

        await rows(wrapper)[1].find('button').trigger('click')
        await flushPromises()

        expect(wrapper.text()).toContain('Edit EMAIL')
        expect((fields(wrapper)[0].element as HTMLInputElement).value).toBe('EMAIL')
        expect((fields(wrapper)[1].element as HTMLInputElement).value).toBe('2')

        await fields(wrapper)[3].setValue('45')
        await submit(wrapper)
        await flushPromises()

        expect(JSON.parse(sent[0].body as string)).toMatchObject({
            name: 'EMAIL',
            field: '2',
            width: '45',
        })
        // Saved, so the form is adding again rather than still editing EMAIL.
        expect(wrapper.text()).toContain('Add a dictionary entry')
    })

    it('deletes an entry after confirming, and re-reads what is left', async () => {
        const wrapper = await openUsers(View)

        vi.stubGlobal(
            'fetch',
            stubServer({
                '/api/accounts/SALES/files/USERS/dictionary': dictionaryList([dictionary[0]]),
            }),
        )
        await rows(wrapper)[1].findAll('button')[1].trigger('click')
        await flushPromises()

        expect(window.confirm).toHaveBeenCalledWith(
            'Delete the dictionary entry "EMAIL"? The field\'s data stays.',
        )
        expect(sent).toEqual([
            {
                method: 'DELETE',
                path: '/api/accounts/SALES/files/USERS/dictionary/EMAIL',
                body: undefined,
            },
        ])
        expect(rows(wrapper)).toHaveLength(1)
    })

    it('reports the database’s own words when a definition is refused', async () => {
        // The attributes are judged by SET.DICT, not here, so a refusal has to
        // survive the reload that follows it rather than being cleared by it.
        vi.stubGlobal(
            'fetch',
            stubServer(
                {},
                {
                    'POST /api/accounts/SALES/files/USERS/dictionary':
                        'Attribute number must be 1 or greater',
                },
            ),
        )
        const wrapper = await openUsers(View)

        await fields(wrapper)[0].setValue('PRICE')
        await fields(wrapper)[1].setValue('0')
        await submit(wrapper)
        await flushPromises()

        expect(alerts.message.value).toBe('Attribute number must be 1 or greater')
        // The form still holds what was typed, so it can be corrected.
        expect((fields(wrapper)[0].element as HTMLInputElement).value).toBe('PRICE')
    })

    it('abandons an open edit when a different file is selected', async () => {
        const wrapper = await openUsers(View)
        await rows(wrapper)[1].find('button').trigger('click')
        await flushPromises()
        expect(wrapper.text()).toContain('Edit EMAIL')

        await selectors(wrapper, 1)[0].trigger('click')
        await flushPromises()

        expect(wrapper.text()).toContain('Dictionary of SALES/DIR')
        expect(wrapper.text()).not.toContain('Edit EMAIL')
        expect((fields(wrapper)[0].element as HTMLInputElement).value).toBe('')
    })
})
