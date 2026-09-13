<script lang="ts" setup>
/**
 * Picks the capabilities a client is granted.
 *
 * Checkboxes rather than a free-text list: the set is closed and small, the
 * server refuses an unknown name outright, and a mistyped capability that came
 * back as an error would be a worse way to learn the same thing.
 */
import {CAPABILITIES} from '../capabilities'

const selected = defineModel<string[]>({required: true})
/** Whether ADMIN is set, which already carries every capability. */
defineProps<{disabled?: boolean}>()

function toggle(name: string, on: boolean): void {
  selected.value = on ? [...selected.value, name] : selected.value.filter((held) => held !== name)
}
</script>

<template>
  <fieldset class="capabilities" :disabled="disabled">
    <legend>Capabilities</legend>
    <p v-if="disabled" class="hint">An administrator already holds every capability.</p>
    <label v-for="capability in CAPABILITIES" :key="capability.name" class="check">
      <input
        :checked="selected.includes(capability.name)"
        type="checkbox"
        @change="toggle(capability.name, ($event.target as HTMLInputElement).checked)"
      />
      <span>
        {{ capability.label }}
        <code>{{ capability.name }}</code>
        <small>{{ capability.detail }}</small>
      </span>
    </label>
  </fieldset>
</template>
