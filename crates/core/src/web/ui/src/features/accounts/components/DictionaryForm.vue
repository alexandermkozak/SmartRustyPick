<script lang="ts" setup>
/**
 * Adds a dictionary entry, or replaces the one being edited.
 *
 * One form for both, because `SET.DICT` makes no distinction: an entry is
 * stored under its name, and storing it again replaces it. Naming an entry that
 * already exists is therefore an edit, which is worth saying on screen rather
 * than discovering.
 */
import {computed} from 'vue'
import type {DictionaryDraft} from '../types'

const props = defineProps<{
  draft: DictionaryDraft
  entries: string[]
  editing: string | null
  saving: boolean
}>()
defineEmits<{submit: []; cancel: []}>()

const replacing = computed(() => {
  const name = props.draft.name.trim()
  return name !== '' && name !== props.editing && props.entries.includes(name)
})
</script>

<template>
  <form class="card form" @submit.prevent="$emit('submit')">
    <h3>{{ editing ? `Edit ${editing}` : 'Add a dictionary entry' }}</h3>
    <label>
      Name
      <input
        v-model="draft.name"
        autocomplete="off"
        placeholder="PRICE"
        required
        title="The name a query uses for this field"
      />
    </label>
    <label>
      Attribute number
      <input
        v-model="draft.field"
        autocomplete="off"
        inputmode="numeric"
        placeholder="1"
        required
        title="Which attribute of the record this field is, counting from 1"
      />
    </label>
    <label>
      Heading
      <input v-model="draft.heading" autocomplete="off" placeholder="defaults to the name" />
    </label>
    <label>
      Justification
      <select v-model="draft.justification">
        <option value="L">Left</option>
        <option value="R">Right</option>
      </select>
    </label>
    <label>
      Display width
      <input v-model="draft.width" autocomplete="off" inputmode="numeric" placeholder="10" />
    </label>
    <label>
      Conversion
      <input v-model="draft.conversion" autocomplete="off" placeholder="MD2" />
    </label>
    <p class="note">
      The width and heading affect how <code>LIST</code> lays the field out. A conversion of
      <code>MD2</code> stores <code>12.34</code> as <code>1234</code> and reads it back with the
      point put in.
    </p>
    <p v-if="replacing" class="hint">
      {{ draft.name.trim() }} already exists; saving replaces its definition.
    </p>
    <div class="inline-form">
      <button :disabled="saving" type="submit">
        {{ saving ? 'Saving…' : editing ? 'Save changes' : 'Add entry' }}
      </button>
      <button v-if="editing" class="small" type="button" @click="$emit('cancel')">Cancel</button>
    </div>
  </form>
</template>
