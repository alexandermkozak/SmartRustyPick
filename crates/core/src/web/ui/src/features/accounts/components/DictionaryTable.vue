<script lang="ts" setup>
/**
 * A file's dictionary entries: what each field is called, where it sits in the
 * record, and how it is rendered.
 *
 * The raw definition is shown as a title rather than a column. It is the whole
 * truth about an entry - including anything at a position this table does not
 * name - but it is unreadable next to the columns that say the same thing.
 */
import type {DictionaryEntry} from '../types'

defineProps<{entries: DictionaryEntry[]; loaded: boolean; editing: string | null; busy: boolean}>()
const emit = defineEmits<{edit: [entry: DictionaryEntry]; drop: [name: string]}>()

/**
 * The controlling field, and the tier it pairs on when that is the second one.
 * `V` is the default and adds nothing to read, so only `S` is spelled out.
 */
function association(entry: DictionaryEntry): string {
  if (!entry.association) return '—'
  return entry.associationDepth === 'S' ? `${entry.association} (sub-values)` : entry.association
}

function drop(name: string): void {
  if (!window.confirm(`Delete the dictionary entry "${name}"? The field's data stays.`)) return
  emit('drop', name)
}
</script>

<template>
  <p v-if="!loaded" class="empty">Loading…</p>
  <p v-else-if="!entries.length" class="empty">
    This file has no dictionary entries, so a query has no field names to work with.
  </p>
  <div v-else class="table-wrap">
    <table>
      <thead>
        <tr>
          <th>Name</th>
          <th class="num">Attribute</th>
          <th>Heading</th>
          <th>Justify</th>
          <th class="num">Width</th>
          <th>Associated with</th>
          <th>Conversion</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="entry in entries" :key="entry.name" :aria-current="editing === entry.name">
          <td :title="entry.definition" class="mono">{{ entry.name }}</td>
          <td class="num">{{ entry.field ?? '—' }}</td>
          <td>{{ entry.heading || '—' }}</td>
          <td>{{ entry.justification || '—' }}</td>
          <td class="num">{{ entry.width ?? '—' }}</td>
          <td class="mono">{{ association(entry) }}</td>
          <td class="mono">{{ entry.conversion || '—' }}</td>
          <td class="actions">
            <button :disabled="busy" class="small" type="button" @click="$emit('edit', entry)">
              Edit
            </button>
            <button :disabled="busy" class="small danger" type="button" @click="drop(entry.name)">
              Delete
            </button>
          </td>
        </tr>
      </tbody>
    </table>
  </div>
</template>
