<script lang="ts" setup>
/**
 * Picks a dictionary field to index.
 *
 * A select rather than a text box: an index is on a field the dictionary
 * already defines, and the list of fields is right there on the page. Typing a
 * name here could only ever produce a refusal the page could have avoided
 * asking for.
 */
const props = defineProps<{
  /** Dictionary fields that are defined and not indexed yet. */
  candidates: string[]
  modelValue: string
  saving: boolean
  /** Whether the file has a dictionary at all; nothing can be indexed without one. */
  hasDictionary: boolean
}>()
defineEmits<{submit: []; 'update:modelValue': [field: string]}>()
</script>

<template>
  <form class="card form new-index" @submit.prevent="$emit('submit')">
    <h3>Index a field</h3>
    <p v-if="!hasDictionary" class="note">
      This file has no dictionary entries. An index is on a named field, so define one first.
    </p>
    <p v-else-if="!props.candidates.length" class="note">
      Every field this file defines is already indexed.
    </p>
    <template v-else>
      <label>
        Field
        <select
          :value="modelValue"
          title="A dictionary field of this file"
          @change="$emit('update:modelValue', ($event.target as HTMLSelectElement).value)"
        >
          <option value="">Choose a field…</option>
          <option v-for="name in props.candidates" :key="name" :value="name">{{ name }}</option>
        </select>
      </label>
      <p class="note">
        Building the index reads the file once. After that it is maintained on every write to the
        field, and <code>WITH {{ modelValue || 'FIELD' }} = …</code> resolves through it instead of
        reading every record.
      </p>
      <div class="inline-form">
        <button :disabled="saving || !modelValue" type="submit">
          {{ saving ? 'Building…' : 'Create index' }}
        </button>
      </div>
    </template>
  </form>
</template>
