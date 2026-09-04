<script lang="ts" setup>
/**
 * One file: what it is made of, whether that is healthy, and the one thing
 * about it that is settable.
 *
 * The layout answers the questions in the order an administrator has them.
 * *Is this file all right* comes first, as verdicts with the rule behind each
 * one, because a table of thirteen numbers with no evaluation of any of them
 * leaves the reader to decide whether four megabytes against ninety-six
 * kilobytes is fine. *What is it made of* comes second. The distribution comes
 * between them, because skew is a shape and two extremes cannot show one.
 *
 * No threshold is decided here. The verdicts come from the database, which is
 * also what the CLI prints, so this page and that one cannot disagree about
 * where the line is.
 */
import {computed} from 'vue'
import DistributionChart from '@shared/components/DistributionChart.vue'
import HealthPill from '@shared/components/HealthPill.vue'
import HealthTable from '@shared/components/HealthTable.vue'
import StatList from '@shared/components/StatList.vue'
import {bytes, count, duration} from '@shared/format'
import {NO_HEALTH, verdictLabel} from '@shared/health'
import type {FileStats} from '../types'

const props = defineProps<{stats: FileStats | null; changing: boolean}>()
const emit = defineEmits<{setDurable: [durable: boolean]}>()

const health = computed(() => props.stats?.health ?? NO_HEALTH)

/**
 * The indexes as one line: how many, and whether any of them has fallen behind
 * the records. The table below the dictionary says the rest; this is the row
 * that makes a stale index visible from the panel a person is already reading.
 */
function indexSummary(file: FileStats): string {
  const indexes = file.indexes ?? []
  if (!indexes.length) return 'none'
  const stale = indexes.filter((index) => index.stale).length
  const fields = indexes.map((index) => index.field).join(', ')
  return stale ? `${fields} (${stale} stale)` : fields
}

/** Records per group as one row, since the chart below draws the rest of it. */
function spread(file: FileStats): string {
  const groups = file.group_records
  if (!groups || !groups.groups) return '—'
  return (
    `${count(groups.min)} / ${count(groups.median)} / ` +
    `${groups.mean.toFixed(1)} / ${count(groups.max)}`
  )
}

/** How far the file is from the full rewrite a modulus change is. */
function headroom(file: FileStats): string {
  if (file.records_until_growth === undefined) return '—'
  const shrink =
    file.records_until_shrink === null || file.records_until_shrink === undefined
      ? 'no shrink from here'
      : `${count(file.records_until_shrink)} fewer halves it`
  return `${count(file.records_until_growth)} more records doubles it; ${shrink}`
}

const rows = computed<Array<[string, string]>>(() => {
  const file = props.stats
  if (!file) return []
  return [
    ['Records', count(file.record_count)],
    ['Dictionary entries', count(file.dict_count)],
    ['Indexes', indexSummary(file)],
    ['Hash modulus', count(file.modulus)],
    ['Group files', count(file.group_count)],
    ['Records per group', `${spread(file)} (min / median / mean / max)`],
    ['Headroom', headroom(file)],
    ['Smallest group', bytes(file.smallest_group_bytes)],
    ['Largest group', bytes(file.largest_group_bytes)],
    ['On disk', bytes(file.disk_bytes)],
    ['Flush version', count(file.version)],
    ['Durable writes', file.durable ? 'yes' : 'no'],
    ['In memory', file.loaded ? 'yes' : 'no'],
    [
      'Last modified',
      file.modified_seconds_ago === null ? '—' : `${duration(file.modified_seconds_ago)} ago`,
    ],
  ]
})

// DIR holds the flags rather than carrying one, so it is the one file the
// database refuses to set. Saying so beats offering a button that always fails.
const settable = computed(() => props.stats !== null && props.stats.name !== 'DIR')

function toggle(): void {
  if (!props.stats) return
  emit('setDurable', !props.stats.durable)
}
</script>

<template>
  <h2>File statistics</h2>
  <div class="card">
    <p v-if="!stats" class="empty">Select a file.</p>
    <template v-else>
      <h3 class="mono">
        {{ stats.account }}/{{ stats.name }}
        <HealthPill :verdict="health.verdict" show-good />
      </h3>

      <h4>Health — {{ verdictLabel(health.verdict) }}</h4>
      <HealthTable :health="health" />

      <template v-if="stats.group_records && stats.group_records.groups">
        <h4>Records per group</h4>
        <DistributionChart
          :buckets="stats.group_records.buckets"
          :groups="stats.group_records.groups"
        />
        <p class="note">
          {{ count(stats.group_records.empty) }} of {{ count(stats.group_records.groups) }} groups
          hold nothing; {{ count(stats.group_records.overweight) }} hold more than twice the mean.
          <template v-if="stats.group_records.unreadable">
            {{ count(stats.group_records.unreadable) }} predate the checksum trailer and could not
            be counted.
          </template>
        </p>
      </template>

      <h4>Layout</h4>
      <StatList :rows="rows" />

      <div v-if="settable" class="file-actions">
        <button :disabled="changing" class="small" type="button" @click="toggle">
          {{ stats.durable ? 'Buffer writes' : 'Make durable' }}
        </button>
        <p class="note">
          {{
            stats.durable
              ? 'Buffering returns this file to the database’s flush policy: a write may stay in memory briefly after it is acknowledged.'
              : 'Durable flushes every write to this file to disk before acknowledging it, and flushes what it still has buffered now.'
          }}
        </p>
      </div>
      <p v-else class="note">
        DIR carries the durability flags; its own writes are always flushed.
      </p>
    </template>
  </div>
</template>
