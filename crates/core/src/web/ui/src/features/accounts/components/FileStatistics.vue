<script lang="ts" setup>
/** One file's layout and cost, and the one thing about it that is settable. */
import {computed} from 'vue'
import StatList from '@shared/components/StatList.vue'
import {bytes, count, duration} from '@shared/format'
import type {FileStats} from '../types'

const props = defineProps<{stats: FileStats | null; changing: boolean}>()
const emit = defineEmits<{setDurable: [durable: boolean]}>()

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
      <h3 class="mono">{{ stats.account }}/{{ stats.name }}</h3>
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
