/**
 * The accounts slice, tested through its view.
 *
 * A slice owning its own tests is the other half of owning its own code: this
 * file moves, or is deleted, with the feature it covers.
 */

import {afterEach, beforeEach, describe, expect, it, vi} from 'vitest'
import {flushPromises, mount, type VueWrapper} from '@vue/test-utils'
import type {Component} from 'vue'
import type {Health, Measure, Verdict} from '@shared/health'
import type {AccountStats, DictionaryEntry, FileStats, IndexReport, IndexStats} from './types'

/**
 * A health object as the server sends one.
 *
 * The verdicts and the wording are the database's: the page renders what it is
 * given rather than deciding anything, which is the property these fixtures are
 * shaped to test. A fixture that re-derived a verdict here would be asserting
 * that the page has a rule, which is exactly what it must not have.
 */
const measure = (id: string, verdict: Verdict, value: string, detail: string): Measure => ({
    id,
    label: id.replace(/_/g, ' '),
    value,
    verdict,
    threshold: `the rule behind ${id}`,
    detail,
})

const health = (...measures: Measure[]): Health => ({
    verdict: measures.reduce<Verdict>(
        (worst, m) =>
            m.verdict === 'act' || worst === 'act'
                ? 'act'
                : m.verdict === 'watch'
                  ? 'watch'
                  : worst,
        'good',
    ),
    measures,
})

const healthy = health(measure('skew', 'good', '1.2x', 'Records are spread evenly.'))

const account: AccountStats = {
    name: 'SALES',
    directory: 'db_storage/SALES',
    file_count: 2,
    record_count: 1280,
    disk_bytes: 262144,
    index_count: 1,
    stale_indexes: 0,
    unhealthy_files: 0,
    health: {verdict: 'good', reasons: []},
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
    indexes: [],
    group_bytes: 212992,
    index_bytes: 40960,
    records_per_group_target: 16,
    load_factor: 0.625,
    records_until_growth: 769,
    records_until_shrink: 768,
    largest_group_share: 0.021,
    skew: 2.7,
    group_records: {
        groups: 128,
        min: 3,
        max: 27,
        mean: 10,
        median: 10,
        empty: 0,
        overweight: 4,
        unreadable: 0,
        buckets: [
            {min: 3, max: 12, groups: 120},
            {min: 13, max: 27, groups: 8},
        ],
    },
    health: healthy,
}

// The real envelope: the server omits every field the command did not populate,
// so a stub that spells them out as nulls would not catch a reader that breaks
// on an absent one.
const envelope = (body: Record<string, unknown>) => ({status: 'OK', ...body})

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

const fileList = (durable: boolean, usersHealth = 'good', reasons: string[] = []) =>
    envelope({
        keys: ['DIR', 'USERS'],
        results: [
            ['DIR', {durable: false, health: 'good', health_reasons: []}],
            ['USERS', {durable, health: usersHealth, health_reasons: reasons}],
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

const emailIndex: IndexStats = {
    file: 'USERS',
    field: 'EMAIL',
    attribute: 2,
    values: 1200,
    postings: 1280,
    largest_postings: 3,
    modulus: 128,
    version: 7,
    group_count: 128,
    disk_bytes: 40960,
    data_version: 42,
    stale: false,
    loaded: true,
    built_seconds_ago: 90,
    excluded: [],
    usage: {
        lookups: 812,
        candidates: 866,
        matched: 840,
        measured_lookups: 800,
        excluded_lookups: 0,
    },
    health: health(
        measure(
            'selectivity',
            'good',
            '1.1',
            'An average lookup narrows 1,280 records to about 1.',
        ),
    ),
}

/** `INDEX.STATS` for one index: the values that dominate it. */
const emailReport = (index: IndexStats = emailIndex): IndexReport => ({
    record_count: 1280,
    index,
    values_available: true,
    top_values: [
        {value: 'ACTIVE', keys: 900},
        {value: '', keys: 200},
        {value: 'PENDING', keys: 180},
    ],
})

const indexList = (entries: IndexStats[]) =>
    envelope({
        keys: entries.map((entry) => entry.field),
        results: entries.map((entry) => [entry.field, entry]),
        count: entries.length,
    })

const routes = {
    '/api/accounts': envelope({results: [['SALES', account]], count: 1}),
    '/api/accounts/SALES/files': fileList(false),
    '/api/accounts/SALES/files/USERS': envelope({record: fileStats}),
    '/api/accounts/SALES/files/USERS/dictionary': dictionaryList(),
    '/api/accounts/SALES/files/DIR/dictionary': dictionaryList([]),
    '/api/accounts/SALES/files/USERS/indexes': indexList([]),
    '/api/accounts/SALES/files/DIR/indexes': indexList([]),
    '/api/accounts/SALES/files/USERS/indexes/EMAIL': envelope({record: emailReport()}),
    '/api/accounts/SALES/indexes': indexList([]),
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

    it('shows what the database makes of a file, not only what it is made of', async () => {
        // The panel used to render thirteen rows with no evaluation of any of
        // them, leaving the reader to decide whether four megabytes against
        // ninety-six kilobytes was fine.
        const unwell = {
            ...fileStats,
            legacy: true,
            checksums: false,
            health: health(
                measure('format', 'act', 'legacy flat file', 'The next flush converts it.'),
                measure(
                    'skew',
                    'watch',
                    '4.1x',
                    'The largest group holds 27 against a mean of 10.',
                ),
            ),
        }
        vi.stubGlobal(
            'fetch',
            stubFetch({...routes, '/api/accounts/SALES/files/USERS': envelope({record: unwell})}),
        )
        const wrapper = await openUsers(View)

        // The verdict, its wording and the rule behind it - all the server's.
        expect(wrapper.text()).toContain('needs attention')
        expect(wrapper.text()).toContain('The next flush converts it.')
        expect(wrapper.text()).toContain('the rule behind format')
        // Worst first, so the row to act on is the one read first.
        const measures = wrapper.findAll('.measure')
        expect(measures[0].classes()).toContain('act')

        // And the layout is still there, with the two figures that were only
        // ever available by opening the file: the spread and the headroom.
        expect(wrapper.text()).toContain('Records per group')
        expect(wrapper.text()).toContain('3 / 10 / 10.0 / 27')
        expect(wrapper.text()).toContain('769 more records doubles it')
        expect(wrapper.find('.distribution').exists()).toBe(true)
    })

    it('marks the file that needs attention in the list, so it can be found', async () => {
        vi.stubGlobal(
            'fetch',
            stubFetch({
                ...routes,
                '/api/accounts/SALES/files': fileList(false, 'act', ['1 of 2 indexes stale']),
            }),
        )
        const wrapper = mount(View)
        await flushPromises()
        await selectors(wrapper, 0)[0].trigger('click')
        await flushPromises()

        const entries = wrapper.findAll('.list')[1].findAll('li')
        expect(entries[0].find('.tag.verdict').exists()).toBe(false)
        expect(entries[1].find('.tag.verdict.act').exists()).toBe(true)
        expect(entries[1].find('.tag.verdict').attributes('title')).toBe('1 of 2 indexes stale')
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

        expect(sent).toEqual([
            {method: 'POST', path: '/api/accounts', body: '{"name":"REPORTS","demo":false}'},
        ])
        // Re-read straight after, so a created account appears now rather than
        // whenever the twenty-second poll next comes round.
        expect(traffic).toEqual(['GET /api/accounts', 'POST /api/accounts', 'GET /api/accounts'])
    })

    it('creates the demo account through the same field, asking for the fixture', async () => {
        const wrapper = mount(View)
        await flushPromises()

        await wrapper.find('.new-account input').setValue('DEMO')
        await wrapper.findAll('.new-account button')[1].trigger('click')
        await flushPromises()

        // One endpoint, one flag: the page is asking for an account either way.
        expect(sent).toEqual([
            {method: 'POST', path: '/api/accounts', body: '{"name":"DEMO","demo":true}'},
        ])
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

        // The demo account is refused the same way, being the same endpoint.
        await wrapper.find('.new-account input').setValue('DEMO')
        await wrapper.findAll('.new-account button')[1].trigger('click')
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

describe('the indexes of an account', () => {
    let View: Component

    /** The account-wide panel, a root-level sibling like the others. */
    const panel = (wrapper: VueWrapper) => wrapper.find('.account-indexes')

    const badIndex: IndexStats = {
        ...emailIndex,
        file: 'ORDERS',
        field: 'STATUS',
        values: 3,
        largest_postings: 1164,
        usage: {lookups: 0, candidates: 0, matched: 0, measured_lookups: 0, excluded_lookups: 0},
        health: health(
            measure('dominant_value', 'act', '91%', 'One value covers 91% of the file.'),
            measure(
                'usage',
                'watch',
                '0',
                'No query has used this index since the server started.',
            ),
        ),
    }

    beforeEach(async () => {
        vi.resetModules()
        reset()
        View = (await import('./AccountsView.vue')).default
    })

    afterEach(() => {
        vi.unstubAllGlobals()
        vi.restoreAllMocks()
    })

    it('appears with the account rather than waiting for a file to be opened', async () => {
        // The gap this closes: nothing reported on an index unless somebody
        // opened the page for its file, so a database with forty files had no
        // view saying which three were worth looking at.
        vi.stubGlobal('fetch', stubServer({'/api/accounts/SALES/indexes': indexList([badIndex])}))
        const wrapper = mount(View)
        await flushPromises()
        expect(wrapper.find('.account-indexes').exists()).toBe(false)

        await selectors(wrapper, 0)[0].trigger('click')
        await flushPromises()
        expect(panel(wrapper).text()).toContain('Indexes in SALES')
        // Named by file and field, because the point is to say which file to open.
        expect(panel(wrapper).text()).toContain('ORDERS/STATUS')
        expect(panel(wrapper).text()).toContain('One value covers 91% of the file.')
        expect(panel(wrapper).find('.tag.verdict.act').exists()).toBe(true)
    })

    it('lists only what needs attention, not every index in the account', async () => {
        // A table of all forty would be the same wall of numbers one level
        // further out. The file's own table is where the rest live.
        vi.stubGlobal(
            'fetch',
            stubServer({
                '/api/accounts/SALES/indexes': indexList([
                    badIndex,
                    {...emailIndex, health: health(measure('selectivity', 'good', '1.1', 'Fine.'))},
                ]),
            }),
        )
        const wrapper = mount(View)
        await flushPromises()
        await selectors(wrapper, 0)[0].trigger('click')
        await flushPromises()

        expect(panel(wrapper).text()).toContain('2 indexes across 2 files')
        expect(panel(wrapper).text()).toContain('1 need attention')
        expect(panel(wrapper).findAll('.list li')).toHaveLength(1)
        expect(panel(wrapper).text()).not.toContain('USERS/EMAIL')
    })

    it('opens the file the bad index belongs to', async () => {
        vi.stubGlobal('fetch', stubServer({'/api/accounts/SALES/indexes': indexList([badIndex])}))
        const wrapper = mount(View)
        await flushPromises()
        await selectors(wrapper, 0)[0].trigger('click')
        await flushPromises()

        // ORDERS is not in the file list stub, so the request is what is
        // asserted: the row is a way through to the file, not a dead end.
        reset()
        await panel(wrapper).find('.list button.select').trigger('click')
        await flushPromises()
        expect(traffic).toContain('GET /api/accounts/SALES/files/ORDERS')
    })

    it('says what an account with no indexes costs, rather than showing an empty table', async () => {
        vi.stubGlobal('fetch', stubServer())
        const wrapper = mount(View)
        await flushPromises()
        await selectors(wrapper, 0)[0].trigger('click')
        await flushPromises()
        expect(panel(wrapper).text()).toContain('No file in this account carries a secondary index')
    })
})

describe('the indexes of a file', () => {
    let View: Component
    let alerts: {message: {value: string | null}}

    /** The index section. Like the dictionary, it is a root-level sibling. */
    const panel = (wrapper: VueWrapper) => wrapper.find('.indexes')
    const rows = (wrapper: VueWrapper) => panel(wrapper).findAll('tbody tr')

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

    it('appears only once a file is selected, and says what a file with none costs', async () => {
        const wrapper = mount(View)
        await flushPromises()
        expect(wrapper.find('.indexes').exists()).toBe(false)

        const opened = await openUsers(View)
        expect(opened.text()).toContain('Indexes on SALES/USERS')
        expect(panel(opened).text()).toContain('This file has no indexes')
        expect(panel(opened).text()).toContain('1,280')
    })

    it('shows the counts an index is judged on, and how much it is used', async () => {
        vi.stubGlobal(
            'fetch',
            stubServer({
                '/api/accounts/SALES/files/USERS/indexes': indexList([emailIndex]),
            }),
        )
        const wrapper = await openUsers(View)

        const cells = rows(wrapper)[0].findAll('td')
        expect(cells[0].text()).toContain('EMAIL')
        expect(cells[1].text()).toBe('2')
        expect(cells[2].text()).toBe('1,200')
        expect(cells[3].text()).toBe('1,280')
        expect(cells[4].text()).toBe('1.1')
        expect(cells[5].text()).toBe('3')
        // Lookups served: the number that says whether anything is querying it
        // at all, which no shape over the data can answer.
        expect(cells[6].text()).toBe('812')
    })

    it('renders the database’s verdict rather than deciding one of its own', async () => {
        // The page used to hold its own thresholds - a lookup narrowing to two
        // records is "close to unique", a largest posting list of a quarter of
        // the file is worth warning about - which the CLI did not share. The
        // rule now lives in the engine, and this asserts the page says what it
        // was told and nothing else.
        const dominated = {
            ...emailIndex,
            values: 2,
            postings: 1280,
            largest_postings: 900,
            health: health(
                measure(
                    'dominant_value',
                    'act',
                    '70%',
                    'One value covers 70% of the file. Read the value histogram and exclude it.',
                ),
            ),
        }
        vi.stubGlobal(
            'fetch',
            stubServer({
                '/api/accounts/SALES/files/USERS/indexes': indexList([dominated]),
                '/api/accounts/SALES/files/USERS/indexes/EMAIL': envelope({
                    record: emailReport(dominated),
                }),
            }),
        )
        const wrapper = await openUsers(View)

        // The verdict is visible on the row before anything is opened.
        expect(rows(wrapper)[0].find('.tag.verdict.act').exists()).toBe(true)

        await rows(wrapper)[0].findAll('button')[0].trigger('click')
        await flushPromises()
        expect(panel(wrapper).text()).toContain('One value covers 70% of the file')
        // And the rule behind it, so the number is arguable rather than oracular.
        expect(panel(wrapper).text()).toContain('the rule behind dominant_value')
    })

    it('names the value that dominates an index, and offers to stop indexing it', async () => {
        vi.stubGlobal(
            'fetch',
            stubServer({
                '/api/accounts/SALES/files/USERS/indexes': indexList([emailIndex]),
            }),
        )
        const wrapper = await openUsers(View)

        // The values are their own request: the listing is read on every
        // navigation and stays cheap, so this is asked for deliberately.
        expect(traffic).not.toContain('GET /api/accounts/SALES/files/USERS/indexes/EMAIL')
        await rows(wrapper)[0].findAll('button')[0].trigger('click')
        await flushPromises()
        expect(traffic).toContain('GET /api/accounts/SALES/files/USERS/indexes/EMAIL')

        // Largest first, with its share of the file - "STATUS = ACTIVE is 70%
        // of it" rather than "this index is skewed".
        const values = panel(wrapper).findAll('.value-histogram tbody tr')
        expect(values[0].text()).toContain('ACTIVE')
        expect(values[0].text()).toContain('900')
        expect(values[0].text()).toContain('70%')
        // The empty value is a value like any other and is named, not blank.
        expect(values[1].text()).toContain('(the empty value)')

        // Acting on the diagnosis without leaving the page for the CLI.
        reset()
        await values[0].find('button').trigger('click')
        await flushPromises()
        expect(sent[0]).toEqual({
            method: 'POST',
            path: '/api/accounts/SALES/files/USERS/indexes/EMAIL/exclude',
            body: JSON.stringify({values: ['ACTIVE']}),
        })
        // The histogram is what has just changed, so it is re-read as well as
        // the listing.
        expect(traffic).toContain('GET /api/accounts/SALES/files/USERS/indexes/EMAIL')
    })

    it('keeps the exclusions it already has when another value is added', async () => {
        const excluding = {...emailIndex, excluded: ['']}
        vi.stubGlobal(
            'fetch',
            stubServer({
                '/api/accounts/SALES/files/USERS/indexes': indexList([excluding]),
                '/api/accounts/SALES/files/USERS/indexes/EMAIL': envelope({
                    record: emailReport(excluding),
                }),
            }),
        )
        const wrapper = await openUsers(View)
        await rows(wrapper)[0].findAll('button')[0].trigger('click')
        await flushPromises()
        expect(panel(wrapper).text()).toContain('Not indexed: “(the empty value)”')

        reset()
        await panel(wrapper).findAll('.value-histogram tbody tr')[0].find('button').trigger('click')
        await flushPromises()
        // The command replaces the set, so the page has to send what it wants
        // kept as well as what it is adding.
        expect(sent[0].body).toBe(JSON.stringify({values: ['', 'ACTIVE']}))
    })

    it('will not read the values of an index that does not match the records', async () => {
        const stale = {...emailIndex, stale: true}
        vi.stubGlobal(
            'fetch',
            stubServer({
                '/api/accounts/SALES/files/USERS/indexes': indexList([stale]),
                '/api/accounts/SALES/files/USERS/indexes/EMAIL': envelope({
                    record: {...emailReport(stale), values_available: false, top_values: []},
                }),
            }),
        )
        const wrapper = await openUsers(View)
        await rows(wrapper)[0].findAll('button')[0].trigger('click')
        await flushPromises()

        // An empty histogram would read as an empty index, which is a different
        // and wrong thing to tell somebody.
        expect(panel(wrapper).find('.value-histogram').exists()).toBe(false)
        expect(panel(wrapper).text()).toContain('does not match the records')
    })

    it('marks a stale index and says what to do about it', async () => {
        vi.stubGlobal(
            'fetch',
            stubServer({
                '/api/accounts/SALES/files/USERS/indexes': indexList([
                    {...emailIndex, stale: true},
                ]),
                '/api/accounts/SALES/files/USERS': envelope({
                    record: {...fileStats, indexes: [{...emailIndex, stale: true}]},
                }),
            }),
        )
        const wrapper = await openUsers(View)
        expect(panel(wrapper).find('.tag.stale').exists()).toBe(true)
        // And it is visible from the statistics panel, which is where an
        // operator already is when they are wondering about the file.
        expect(wrapper.text()).toContain('EMAIL (1 stale)')
    })

    it('offers only the dictionary fields that are not indexed yet', async () => {
        vi.stubGlobal(
            'fetch',
            stubServer({
                '/api/accounts/SALES/files/USERS/indexes': indexList([emailIndex]),
            }),
        )
        const wrapper = await openUsers(View)

        const options = panel(wrapper)
            .findAll('.new-index option')
            .map((option) => option.text())
        expect(options).toEqual(['Choose a field…', 'NAME'])
    })

    it('creates an index on the chosen field and re-reads what it now costs', async () => {
        const wrapper = await openUsers(View)
        reset()

        vi.stubGlobal(
            'fetch',
            stubServer({
                '/api/accounts/SALES/files/USERS/indexes': indexList([emailIndex]),
                '/api/accounts/SALES/files/USERS': envelope({
                    record: {...fileStats, indexes: [emailIndex]},
                }),
            }),
        )
        await panel(wrapper).find('.new-index select').setValue('EMAIL')
        await panel(wrapper).find('.new-index').trigger('submit')
        await flushPromises()

        expect(sent).toEqual([
            {
                method: 'POST',
                path: '/api/accounts/SALES/files/USERS/indexes',
                body: '{"field":"EMAIL","values":[]}',
            },
        ])
        // The list and the file's statistics both describe the new index.
        expect(rows(wrapper)).toHaveLength(1)
        expect(wrapper.text()).toContain('EMAIL')
        expect(traffic).toContain('GET /api/accounts/SALES/files/USERS')
    })

    it('rebuilds an index through its own endpoint', async () => {
        vi.stubGlobal(
            'fetch',
            stubServer({
                '/api/accounts/SALES/files/USERS/indexes': indexList([
                    {...emailIndex, stale: true},
                ]),
            }),
        )
        const wrapper = await openUsers(View)
        reset()

        // Values, Rebuild, Drop.
        await rows(wrapper)[0].findAll('button')[1].trigger('click')
        await flushPromises()

        expect(sent).toEqual([
            {
                method: 'POST',
                path: '/api/accounts/SALES/files/USERS/indexes/EMAIL/rebuild',
                body: undefined,
            },
        ])
    })

    it('confirms before dropping an index, and says the records stay', async () => {
        vi.stubGlobal(
            'fetch',
            stubServer({
                '/api/accounts/SALES/files/USERS/indexes': indexList([emailIndex]),
            }),
        )
        const wrapper = await openUsers(View)
        reset()

        await rows(wrapper)[0].findAll('button')[2].trigger('click')
        await flushPromises()

        expect(window.confirm).toHaveBeenCalledWith(
            'Drop the index on "EMAIL"? Queries on it go back to scanning; the records stay.',
        )
        expect(sent).toEqual([
            {
                method: 'DELETE',
                path: '/api/accounts/SALES/files/USERS/indexes/EMAIL',
                body: undefined,
            },
        ])
    })

    it('reports the database’s own words when an index is refused', async () => {
        vi.stubGlobal(
            'fetch',
            stubServer(
                {},
                {
                    'POST /api/accounts/SALES/files/USERS/indexes': 'Admin privileges required',
                },
            ),
        )
        const wrapper = await openUsers(View)

        await panel(wrapper).find('.new-index select').setValue('EMAIL')
        await panel(wrapper).find('.new-index').trigger('submit')
        await flushPromises()

        expect(alerts.message.value).toBe('Admin privileges required')
        expect(panel(wrapper).text()).toContain('This file has no indexes')
    })

    it('says why a file with no dictionary has nothing to index', async () => {
        const wrapper = mount(View)
        await flushPromises()
        await selectors(wrapper, 0)[0].trigger('click')
        await flushPromises()
        await selectors(wrapper, 1)[0].trigger('click')
        await flushPromises()

        expect(wrapper.text()).toContain('Indexes on SALES/DIR')
        expect(panel(wrapper).text()).toContain('An index is on a named field')
        expect(panel(wrapper).find('select').exists()).toBe(false)
    })
})
