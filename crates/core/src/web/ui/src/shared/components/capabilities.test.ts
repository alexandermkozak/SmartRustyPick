/**
 * The capability picker. Getting this wrong grants the wrong authority, which
 * is the kind of bug a page cannot report and an operator cannot see.
 */

import {describe, expect, it} from 'vitest'
import {mount} from '@vue/test-utils'
import CapabilityPicker from './CapabilityPicker.vue'
import {CAPABILITIES} from '../capabilities'

describe('the capability picker', () => {
    it('offers every capability the server knows, by its wire name', () => {
        const wrapper = mount(CapabilityPicker, {props: {modelValue: []}})
        const codes = wrapper.findAll('code').map((el) => el.text())
        expect(codes).toEqual(CAPABILITIES.map((capability) => capability.name))
        expect(codes).toContain('accounts:manage')
    })

    it('adds and removes a capability without disturbing the others', async () => {
        const wrapper = mount(CapabilityPicker, {props: {modelValue: ['server:observe']}})
        const boxes = wrapper.findAll('input[type="checkbox"]')

        await boxes[0].setValue(true)
        expect(wrapper.emitted('update:modelValue')?.at(-1)?.[0]).toEqual([
            'server:observe',
            'accounts:manage',
        ])

        await wrapper.setProps({modelValue: ['server:observe', 'accounts:manage']})
        await wrapper.findAll('input[type="checkbox"]')[2].setValue(false)
        expect(wrapper.emitted('update:modelValue')?.at(-1)?.[0]).toEqual(['accounts:manage'])
    })

    it('is disabled for an administrator, who already holds everything', () => {
        const wrapper = mount(CapabilityPicker, {props: {modelValue: [], disabled: true}})
        expect(wrapper.find('fieldset').attributes('disabled')).toBeDefined()
        expect(wrapper.text()).toContain('already holds every capability')
    })
})
