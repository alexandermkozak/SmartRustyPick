<script lang="ts" setup>
/**
 * A file's secondary indexes: what each costs, what the database makes of it,
 * and what can be done about the one that is not earning its keep.
 *
 * The counts are on the left and the verdict is on the right, because the
 * counts alone never answered the only question an operator has. An index over
 * sixty-four values on a file of twelve hundred records turns a scan of twelve
 * hundred into a lookup of twenty; one over two values turns it into a scan of
 * six hundred, and costs a write every time the field changes.
 *
 * The verdicts are the database's, not this page's. There used to be a
 * `verdict()` here with its own thresholds - a lookup narrowing to two records
 * is "close to unique", a largest posting list of ten and a quarter of the file
 * is worth warning about - which were reasonable guesses that the CLI did not
 * share and could not be improved in one place. Now the rule lives in the
 * engine, beside the numbers it judges, and this renders it.
 */
import {computed} from 'vue'
import HealthPill from '@shared/components/HealthPill.vue'
import HealthTable from '@shared/components/HealthTable.vue'
import {bytes, count, duration} from '@shared/format'
import type {IndexReport, IndexStats} from '../types'

const props = defineProps<{
  indexes: IndexStats[]
  loaded: boolean
  busy: boolean
  /** Records in the file, so a posting count can become a share of it. */
  records: number
  /** The index whose values are open, and what they are. */
  report: IndexReport | null
  reportField: string | null
}>()
const emit = defineEmits<{
  rebuild: [field: string]
  drop: [field: string]
  inspect: [field: string | null]
  exclude: [field: string, values: string[]]
}>()

/** Records the average lookup hands back to the filter behind it. */
const perLookup = (index: IndexStats): number =>
  index.values === 0 ? 0 : index.postings / index.values

const share = (keys: number): number => (props.records > 0 ? keys / props.records : 0)

/** An index value as a person reads it, naming the empty one. */
const spell = (value: string): string => (value === '' ? '(the empty value)' : value)

const rows = computed(() =>
  props.indexes.map((index) => ({
    index,
    average: perLookup(index),
    open: props.reportField === index.field,
  })),
)

/** The values the open index does not hold, as a sentence. */
const excludedSentence = computed(() => {
  const excluded = props.report?.index.excluded ?? []
  if (!excluded.length) return ''
  return excluded.map((value) => `“${spell(value)}”`).join(', ')
})

function drop(field: string): void {
  if (
    !window.confirm(
      `Drop the index on "${field}"? Queries on it go back to scanning; the records stay.`,
    )
  )
    return
  emit('drop', field)
}

/** Adds one value to what the open index skips, keeping the rest. */
function excludeValue(value: string): void {
  const report = props.report
  if (!report) return
  const already = report.index.excluded
  if (already.includes(value)) return
  if (
    !window.confirm(
      `Stop indexing “${spell(value)}” on ${report.index.field}?\n\n` +
        `The index is rebuilt without it, and a query for that value scans exactly as it ` +
        `would with no index at all — the same records, in the same order.`,
    )
  )
    return
  emit('exclude', report.index.field, [...already, value])
}

/** Puts one excluded value back, which is also a rebuild. */
function includeValue(value: string): void {
  const report = props.report
  if (!report) return
  emit(
    'exclude',
    report.index.field,
    report.index.excluded.filter((held) => held !== value),
  )
}

/** Opens one index's values, or closes the one already open. */
function toggle(field: string): void {
  emit('inspect', props.reportField === field ? null : field)
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
          <th class="num">Per lookup</th>
          <th class="num">Largest</th>
          <th class="num">Lookups</th>
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
            <HealthPill
              :title="row.index.health.measures.map((m) => m.label).join(', ')"
              :verdict="row.index.health.verdict"
            />
          </td>
          <td class="num">{{ row.index.attribute }}</td>
          <td class="num">{{ count(row.index.values) }}</td>
          <td class="num">{{ count(row.index.postings) }}</td>
          <td class="num">{{ row.average.toFixed(1) }}</td>
          <td class="num">{{ count(row.index.largest_postings) }}</td>
          <td class="num" title="Lookups served since the server started">
            {{ count(row.index.usage.lookups) }}
          </td>
          <td class="num">{{ bytes(row.index.disk_bytes) }}</td>
          <td>
            {{
              row.index.built_seconds_ago === null
                ? '—'
                : `${duration(row.index.built_seconds_ago)} ago`
            }}
          </td>
          <td class="actions">
            <button class="small" type="button" @click="toggle(row.index.field)">
              {{ row.open ? 'Hide' : 'Values' }}
            </button>
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

    <div v-for="row in rows.filter((r) => r.open)" :key="`${row.index.field}-detail`" class="card">
      <h3 class="mono">{{ row.index.field }}</h3>
      <HealthTable :health="row.index.health" />

      <p v-if="excludedSentence" class="excluded-values">
        Not indexed: {{ excludedSentence }}.
        {{ count(report?.index.usage.excluded_lookups ?? 0) }} lookups have fallen back to a scan
        because of it.
        <button
          v-for="value in report?.index.excluded ?? []"
          :key="value"
          :disabled="busy"
          class="small"
          type="button"
          @click="includeValue(value)"
        >
          Index “{{ spell(value) }}” again
        </button>
      </p>

      <p v-if="!report" class="empty">Reading the values…</p>
      <p v-else-if="!report.values_available" class="empty">
        The values cannot be read: this index does not match the records. Rebuild it first.
      </p>
      <p v-else-if="!report.top_values.length" class="empty">
        This index holds no values, so there is nothing to show.
      </p>
      <table v-else class="value-histogram">
        <thead>
          <tr>
            <th>Value</th>
            <th class="num">Keys</th>
            <th class="share">Share of the file</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="value in report.top_values" :key="value.value">
            <td :class="['mono', {'empty-value': value.value === ''}]">
              {{ spell(value.value) }}
            </td>
            <td class="num">{{ count(value.keys) }}</td>
            <td class="share">
              <span :style="{width: `${Math.max(1, share(value.keys) * 100)}%`}" class="meter" />
              {{ (share(value.keys) * 100).toFixed(0) }}%
            </td>
            <td class="actions">
              <button
                :disabled="busy"
                class="small"
                type="button"
                @click="excludeValue(value.value)"
              >
                Stop indexing
              </button>
            </td>
          </tr>
        </tbody>
      </table>
      <p class="note">
        Excluding a value rebuilds the index without it. Queries for that value scan exactly as they
        would with no index at all, so the answers do not change — what changes is that the longest
        posting list is no longer rewritten on every write that touches it.
      </p>
    </div>
  </div>
</template>
