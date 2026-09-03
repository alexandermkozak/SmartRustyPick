/**
 * The components that render a verdict.
 *
 * Rendering, not judging: every assertion here is that what came out is what
 * went in. A component that decided anything for itself would be a second rule
 * able to drift from the database's.
 */

import {describe, expect, it} from 'vitest'
import {mount} from '@vue/test-utils'
import DistributionChart from './DistributionChart.vue'
import HealthPill from './HealthPill.vue'
import HealthTable from './HealthTable.vue'
import type {Health, Measure, Verdict} from '../health'

const measure = (id: string, verdict: Verdict, value = '1'): Measure => ({
    id,
    label: id,
    value,
    verdict,
    threshold: `the rule behind ${id}`,
    detail: `what ${id} means`,
})

describe('a health pill', () => {
    it('says nothing at all when there is nothing to say', () => {
        // A listing of forty healthy files should not be forty green badges;
        // the pill exists to make the odd one out findable.
        const wrapper = mount(HealthPill, {props: {verdict: 'good'}})
        expect(wrapper.text()).toBe('')
    })

    it('shows a healthy verdict where one was asked for', () => {
        const wrapper = mount(HealthPill, {props: {verdict: 'good', showGood: true}})
        expect(wrapper.text()).toBe('healthy')
        expect(wrapper.classes()).toContain('good')
    })

    it('colours what needs attention differently from what is only watched', () => {
        expect(mount(HealthPill, {props: {verdict: 'watch'}}).classes()).toContain('watch')
        expect(mount(HealthPill, {props: {verdict: 'act'}}).classes()).toContain('act')
        expect(mount(HealthPill, {props: {verdict: 'act'}}).text()).toBe('needs attention')
    })
})

describe('a health table', () => {
    const health: Health = {
        verdict: 'act',
        measures: [
            measure('format', 'good'),
            measure('skew', 'watch'),
            measure('checksums', 'act'),
        ],
    }

    it('puts the row that needs doing something about first', () => {
        const rows = mount(HealthTable, {props: {health}}).findAll('.measure')
        expect(rows.map((row) => row.classes()).map((c) => c.join(' '))).toEqual([
            'measure act',
            'measure watch',
            'measure good',
        ])
    })

    it('shows the rule behind a verdict, but only where there is one to argue with', () => {
        const rows = mount(HealthTable, {props: {health}}).findAll('.measure')
        expect(rows[0].text()).toContain('the rule behind checksums')
        // A healthy row states what it measured; there is nothing to justify.
        expect(rows[2].find('.threshold').exists()).toBe(false)
        expect(rows[2].text()).toContain('what format means')
    })

    it('can show only what is wrong, for a panel with no room for the rest', () => {
        const wrapper = mount(HealthTable, {props: {health, concernsOnly: true}})
        expect(wrapper.findAll('.measure')).toHaveLength(2)
        expect(wrapper.text()).not.toContain('what format means')
    })

    it('says so rather than rendering an empty list', () => {
        expect(mount(HealthTable, {props: {health: null}}).text()).toContain('Nothing to report')
    })
})

describe('the group distribution', () => {
    // The shape is the point: two extremes cannot show whether a file is one
    // long tail or one outlier, and it is the outlier that costs.
    const buckets = [
        {min: 0, max: 9, groups: 28},
        {min: 10, max: 19, groups: 0},
        {min: 90, max: 99, groups: 4},
    ]

    it('draws one column per bucket, tallest at full height', () => {
        const wrapper = mount(DistributionChart, {props: {buckets, groups: 32}})
        const fills = wrapper.findAll('.fill')
        expect(fills).toHaveLength(3)
        expect(fills[0].attributes('style')).toContain('height: 100%')
        // Relative to the tallest column, not to the total: one bucket usually
        // holds most of the groups, and the shape is what is being looked at.
        expect(fills[2].attributes('style')).toContain('height: 14%')
        expect(fills[1].attributes('style')).toContain('height: 0%')
    })

    it('names the range each column covers, so a bar is readable on its own', () => {
        const wrapper = mount(DistributionChart, {props: {buckets, groups: 32}})
        const titles = wrapper.findAll('.bar').map((bar) => bar.attributes('title'))
        expect(titles[0]).toBe('28 groups hold 0–9 records')
        expect(titles[2]).toBe('4 groups hold 90–99 records')
        expect(wrapper.text()).toContain('over 32 groups')
    })

    it('draws nothing for a section with no groups', () => {
        expect(mount(DistributionChart, {props: {buckets: [], groups: 0}}).text()).toBe('')
    })

    it('says "records" rather than a range when a column is one count wide', () => {
        const wrapper = mount(DistributionChart, {
            props: {buckets: [{min: 7, max: 7, groups: 3}], groups: 3},
        })
        expect(wrapper.find('.bar').attributes('title')).toBe('3 groups hold 7 records')
    })
})
