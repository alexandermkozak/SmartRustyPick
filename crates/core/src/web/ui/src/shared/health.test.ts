/**
 * The health module: what a page is allowed to do with a verdict.
 *
 * There is deliberately no threshold here to test. The rules live in the
 * database, beside the numbers they judge, and this file's job is to prove that
 * the page reads what it was told rather than deciding anything itself - so
 * these are about ordering, tolerance of an older server, and what a listing
 * says when there is nothing to say.
 */

import {describe, expect, it} from 'vitest'
import {
    concerns,
    rollUp,
    summarise,
    verdictLabel,
    verdictOf,
    worse,
    type Health,
    type Measure,
    type Verdict,
} from './health'

const measure = (id: string, verdict: Verdict): Measure => ({
    id,
    label: id,
    value: '1',
    verdict,
    threshold: 't',
    detail: 'd',
})

describe('a verdict', () => {
    it('is the worse of any two', () => {
        expect(worse('good', 'watch')).toBe('watch')
        expect(worse('act', 'watch')).toBe('act')
        expect(worse('good', 'good')).toBe('good')
    })

    it('rolls up to the worst of a set, and nothing is healthy', () => {
        expect(rollUp([])).toBe('good')
        expect(rollUp(['good', 'good'])).toBe('good')
        expect(rollUp(['good', 'watch', 'good'])).toBe('watch')
        expect(rollUp(['watch', 'act'])).toBe('act')
    })

    it('reads an older server’s missing verdict as nothing to report', () => {
        // A reply from before the health object existed carries none. Treating
        // that as a problem would be worse than treating it as silence.
        expect(verdictOf(undefined)).toBe('good')
        expect(verdictOf(null)).toBe('good')
        expect(verdictOf('act')).toBe('act')
        expect(verdictOf('watch')).toBe('watch')
        // And a verdict from a newer server this build does not know is not a
        // reason to render something meaningless.
        expect(verdictOf('catastrophic')).toBe('good')
    })

    it('is called something a person reads rather than the wire word', () => {
        expect(verdictLabel('good')).toBe('healthy')
        expect(verdictLabel('watch')).toBe('watch')
        expect(verdictLabel('act')).toBe('needs attention')
    })
})

describe('the concerns of a health object', () => {
    const health: Health = {
        verdict: 'act',
        measures: [
            measure('format', 'good'),
            measure('skew', 'watch'),
            measure('checksums', 'act'),
            measure('load_factor', 'watch'),
        ],
    }

    it('are what is not good, worst first', () => {
        expect(concerns(health).map((m) => m.id)).toEqual(['checksums', 'skew', 'load_factor'])
    })

    it('keep the server’s order within one verdict', () => {
        // The measures arrive in the order they were judged, which reads as a
        // sequence; re-sorting inside a verdict would scramble it.
        const [, first, second] = concerns(health)
        expect([first.id, second.id]).toEqual(['skew', 'load_factor'])
    })

    it('are empty when there is nothing to report, and for no health at all', () => {
        expect(concerns({verdict: 'good', measures: [measure('skew', 'good')]})).toEqual([])
        expect(concerns(null)).toEqual([])
        expect(concerns(undefined)).toEqual([])
    })

    it('summarise to a sentence a panel has room for', () => {
        expect(summarise(health)).toBe('checksums: 1; skew: 1; load_factor: 1')
        expect(summarise({verdict: 'good', measures: []})).toBe('Nothing to do.')
    })
})
