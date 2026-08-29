<script lang="ts" setup>
/** One file's layout and cost. No record ever appears here. */
import {computed} from 'vue'
import StatList from '@shared/components/StatList.vue'
import {bytes, count, duration} from '@shared/format'
import type {FileStats} from '../types'

const props = defineProps<{stats: FileStats | null}>()

const rows = computed<Array<[string, string]>>(() => {
  const file = props.stats
  if (!file) return []
  return [
    ['Records', count(file.record_count)],
    ['Dictionary entries', count(file.dict_count)],
    ['Hash modulus', count(file.modulus)],
    ['Group files', count(file.group_count)],
    ['Smallest group', bytes(file.smallest_group_bytes)],
    ['Largest group', bytes(file.largest_group_bytes)],
    ['On disk', bytes(file.disk_bytes)],
    ['Flush version', count(file.version)],
    ['Durable writes', file.durable ? 'yes' : 'no'],
    ['Checksums', file.checksums ? 'yes' : 'no'],
    ['Format', file.legacy ? 'legacy flat file' : 'hashed'],
    ['In memory', file.loaded ? 'yes' : 'no'],
    [
      'Last modified',
      file.modified_seconds_ago === null ? '—' : `${duration(file.modified_seconds_ago)} ago`,
    ],
  ]
})
</script>

<template>
  <h2>File statistics</h2>
  <div class="card">
    <p v-if="!stats" class="empty">Select a file.</p>
    <template v-else>
      <h3 class="mono">{{ stats.account }}/{{ stats.name }}</h3>
      <StatList :rows="rows" />
    </template>
  </div>
</template>
