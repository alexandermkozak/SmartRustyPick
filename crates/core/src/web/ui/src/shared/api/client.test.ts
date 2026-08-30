/**
 * The envelope unwrappers, against the shape the server actually sends.
 *
 * A field the command did not populate is *absent*, not null - see
 * `docs/protocol.md` and the `skip_serializing_if` attributes on `Response` in
 * `crates/core/src/server/models.rs`. Older servers sent nulls, so both have to
 * mean the same thing to every reader here. A strict `=== null` test would pass
 * against the null spelling and hand `undefined` to a feature against the
 * current one, which is why both spellings are asserted below.
 */

import {afterEach, describe, expect, it, vi} from 'vitest'
import {ApiError, keys, pairs, record} from './client'

/** One JSON reply, whatever the path. */
function stubFetch(body: unknown, status = 200) {
    const stub = vi.fn(async () => new Response(JSON.stringify(body), {status}))
    vi.stubGlobal('fetch', stub)
    return stub
}

afterEach(() => vi.unstubAllGlobals())

describe('record()', () => {
    it('returns the object the command populated', async () => {
        stubFetch({status: 'OK', record: {durable: true}})
        await expect(record('/api/files/USERS')).resolves.toEqual({durable: true})
    })

    it.each([
        ['absent', {status: 'OK'}],
        ['null', {status: 'OK', record: null}],
    ])('fails cleanly when the record is %s', async (_spelling, body) => {
        stubFetch(body)
        await expect(record('/api/files/USERS')).rejects.toBeInstanceOf(ApiError)
    })
})

describe('pairs() and keys()', () => {
    it('return what the command populated', async () => {
        stubFetch({status: 'OK', keys: ['DIR'], results: [['DIR', {durable: false}]], count: 1})
        await expect(pairs('/api/accounts/TEST/files')).resolves.toEqual([
            ['DIR', {durable: false}],
        ])
        await expect(keys('/api/accounts/TEST/files')).resolves.toEqual(['DIR'])
    })

    it.each([
        ['absent', {status: 'OK'}],
        ['null', {status: 'OK', keys: null, results: null}],
    ])('read an %s list as an empty one', async (_spelling, body) => {
        stubFetch(body)
        await expect(pairs('/api/accounts/TEST/files')).resolves.toEqual([])
        await expect(keys('/api/accounts/TEST/files')).resolves.toEqual([])
    })
})

describe('an error status', () => {
    it('becomes an ApiError carrying the server message', async () => {
        stubFetch({error: 'No such file'}, 404)
        await expect(record('/api/files/NOPE')).rejects.toMatchObject({
            status: 404,
            message: 'No such file',
        })
    })
})
