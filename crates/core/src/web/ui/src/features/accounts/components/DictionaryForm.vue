<script lang="ts" setup>
/**
 * Adds a dictionary entry, or replaces the one being edited.
 *
 * One form for both, because `SET.DICT` makes no distinction: an entry is
 * stored under its name, and storing it again replaces it. Naming an entry that
 * already exists is therefore an edit, which is worth saying on screen rather
 * than discovering.
 */
import {computed, watch} from 'vue'
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

/** An entry cannot be its own controller, so it is not offered as one. */
const others = computed(() => props.entries.filter((name) => name !== props.draft.name.trim()))

const associated = computed(() => props.draft.association.trim() !== '')

// A depth without a controlling field is refused, and a group that names no
// tier pairs value for value - which is what `SET.DICT` fills in. The form says
// the same thing rather than sending a blank for the database to default.
watch(associated, (on) => {
  props.draft.associationDepth = on ? props.draft.associationDepth || 'V' : ''
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
      Associated with
      <input
        v-model="draft.association"
        autocomplete="off"
        list="dictionary-fields"
        placeholder="none"
        title="The controlling field this field's values pair with, one for one"
      />
      <datalist id="dictionary-fields">
        <option v-for="name in others" :key="name" :value="name" />
      </datalist>
    </label>
    <label>
      Pairs on
      <select v-model="draft.associationDepth" :disabled="!associated">
        <option value="V">Each value</option>
        <option value="S">Each sub-value</option>
      </select>
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
    <p class="note">
      Associating this field with another makes them one group, so
      <code>BY.EXP</code> explodes them together and value <em>n</em> of one lines up with value
      <em>n</em> of the other. Leave it empty for a field that stands alone; the controlling field
      itself carries nothing.
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
