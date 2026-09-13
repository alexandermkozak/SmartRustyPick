/**
 * How an expiry reads. Getting this wrong is not cosmetic: the whole point of
 * showing an expiry is that somebody reissues before it passes.
 */

import {describe, expect, it} from 'vitest'
import {expiry} from './format'

describe('a certificate expiry', () => {
    it('says unknown when the database never saw the certificate', () => {
        // Authorized by thumbprint alone. No date is invented for it.
        expect(expiry(null, null)).toEqual({text: 'unknown', state: 'unknown'})
        expect(expiry(null, 42)).toEqual({text: 'unknown', state: 'unknown'})
    })

    it('shows the date and the days left', () => {
        expect(expiry('2027-09-13T16:23:45Z', 364)).toEqual({
            text: '2027-09-13 · 364 days',
            state: 'ok',
        })
    })

    it('flags one that is close, and one that has already gone', () => {
        expect(expiry('2026-10-01T00:00:00Z', 18).state).toBe('soon')
        expect(expiry('2026-09-14T00:00:00Z', 0)).toEqual({
            text: '2026-09-14 · today',
            state: 'soon',
        })
        // Never "-3 days": a negative number is a sentence nobody reads
        // correctly at a glance.
        expect(expiry('2026-09-01T00:00:00Z', -12)).toEqual({
            text: '2026-09-01 · expired',
            state: 'expired',
        })
    })

    it('falls back to the date alone when the server sent no day count', () => {
        expect(expiry('2027-09-13T16:23:45Z', null)).toEqual({
            text: '2027-09-13',
            state: 'ok',
        })
    })
})
