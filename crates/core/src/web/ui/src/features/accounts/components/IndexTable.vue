<script lang="ts" setup>
/**
 * A file's secondary indexes, and the analysis that says whether each one is
 * earning its keep.
 *
 * The raw counts are on the left and what they mean is on the right, because
 * the counts alone do not answer the only question an operator has. An index
 * over sixty-four values on a file of twelve hundred records turns a scan of
 * twelve hundred into a lookup of twenty; one over two values turns it into a
 * scan of six hundred, and costs a write every time the field changes.
 */
import {computed} from 'vue'
import {bytes, count, duration} from '@shared/format'
import type {IndexStats} from '../types'

const props = defineProps<{
  indexes: IndexStats[]
  loaded: boolean
  busy: boolean
  /** Records in the file, so a value count can become a selectivity. */
  records: number
}>()
const emit = defineEmits<{rebuild: [field: string]; drop: [field: string]}>()

/** Records the average lookup hands back to the filter behind it. */
const perValue = (index: IndexStats): number =>
  index.values === 0 ? 0 : index.postings / index.values

/**
 * What the numbers add up to, in one sentence per index.
 *
 * Deliberately about the worst case as well as the average: an index whose
 * biggest value covers a quarter of the file is still a scan of a quarter of
 * the file whenever that value is the one asked for.
 */
function verdict(index: IndexStats): string {
  if (index.stale) return 'Does not match the records; rebuild it before it is used again.'
  if (index.values === 0) return 'Empty: nothing in the file carries this field yet.'
  const average = perValue(index)
  const shape =
    average <= 2
      ? 'Close to unique: a lookup lands on one record.'
      : `A lookup narrows the file to about ${Math.round(average)} records.`
  // A share is only worth reporting once the list behind it is long enough for
  // the scan to cost anything: on a file of four records the commonest value
  // covers half of it and that says nothing at all.
  const worst = props.records > 0 ? Math.round((index.largest_postings / props.records) * 100) : 0
  return index.largest_postings >= 10 && worst >= 25
    ? `${shape} Its commonest value still covers ${worst}% of the file, which no index can help with.`
    : shape
}

const rows = computed(() =>
  props.indexes.map((index) => ({
    index,
    average: perValue(index),
    verdict: verdict(index),
  })),
)

function drop(field: string): void {
  if (
    !window.confirm(
      `Drop the index on "${field}"? Queries on it go back to scanning; the records stay.`,
    )
  )
    return
  emit('drop', field)
}
</script>

<template>
  <p v-if="!loaded" class="empty">Loading…</p>
  <p v-else-if="!indexes.length" class="empty">
    This file has no indexes, so every selection other than by key reads all
    {{ count(records) }} of its records.
  </p>
  <div v-else class="table-wrap">
    <table>
      <thead>
        <tr>
          <th>Field</th>
          <th class="num">Attribute</th>
          <th class="num">Values</th>
          <th class="num">Keys indexed</th>
          <th class="num">Per value</th>
          <th class="num">Largest</th>
          <th class="num">On disk</th>
          <th>Built</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="row in rows" :key="row.index.field">
          <td class="mono">
            {{ row.index.field }}
            <span v-if="row.index.stale" class="tag stale" title="Rebuild it before it is used">
              stale
            </span>
          </td>
          <td class="num">{{ row.index.attribute }}</td>
          <td class="num">{{ count(row.index.values) }}</td>
          <td class="num">{{ count(row.index.postings) }}</td>
          <td class="num">{{ row.average.toFixed(1) }}</td>
          <td class="num">{{ count(row.index.largest_postings) }}</td>
          <td class="num">{{ bytes(row.index.disk_bytes) }}</td>
          <td>
            {{
              row.index.built_seconds_ago === null
                ? '—'
                : `${duration(row.index.built_seconds_ago)} ago`
            }}
          </td>
          <td class="actions">
            <button
              :disabled="busy"
              class="small"
              type="button"
              @click="$emit('rebuild', row.index.field)"
            >
              Rebuild
            </button>
            <button
              :disabled="busy"
              class="small danger"
              type="button"
              @click="drop(row.index.field)"
            >
              Drop
            </button>
          </td>
        </tr>
      </tbody>
    </table>
    <ul class="notes">
      <li v-for="row in rows" :key="row.index.field">
        <span class="mono">{{ row.index.field }}</span> — {{ row.verdict }}
      </li>
    </ul>
  </div>
</template>
