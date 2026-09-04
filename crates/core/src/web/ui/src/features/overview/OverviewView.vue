<script lang="ts" setup>
import {computed, onMounted, ref} from 'vue'
import StatGrid from './components/StatGrid.vue'
import StorageHealth from './components/StorageHealth.vue'
import ConnectionsTable from './components/ConnectionsTable.vue'
import {useServerStats} from './composables/useServerStats'
import {overviewApi} from './api'
import {duration, timestamp} from '@shared/format'
import type {AccountHealth} from './types'

const {data, error, loaded} = useServerStats()

const connections = computed(() => data.value?.active_connections ?? [])

// Read once on arrival rather than on the poll loop: the roll-up walks every
// account's files, and a verdict that changes on a flush does not need a
// five-second refresh. A failure here is reported where it happened and leaves
// the rest of the tab alone - the server statistics above are a separate
// request and are still true.
const storage = ref<AccountHealth[]>([])
const storageError = ref<string | null>(null)

onMounted(async () => {
  try {
    storage.value = await overviewApi.storage()
  } catch (cause) {
    storageError.value = cause instanceof Error ? cause.message : String(cause)
  }
})
</script>

<template>
  <section>
    <!-- A failed refresh dims the numbers instead of removing them: stale data
         with a visible reason beats an empty screen. -->
    <p v-if="loaded && error" class="alert inline">{{ error }} — showing the last known values.</p>

    <p v-if="!loaded && error" class="empty error-text">{{ error }}</p>
    <p v-else-if="!loaded || !data" class="empty">Loading…</p>

    <template v-else>
      <StatGrid :snapshot="data" :stale="!!error" />

      <h2>Server</h2>
      <div class="card">
        <dl class="stats">
          <dt>Protocol listener</dt>
          <dd class="mono">{{ data.listen_addr }}</dd>
          <dt>Started</dt>
          <dd>{{ timestamp(data.started_at) }}</dd>
          <dt>Uptime</dt>
          <dd>{{ duration(data.uptime_seconds) }}</dd>
        </dl>
      </div>

      <StorageHealth :accounts="storage" :error="storageError" />

      <h2>Active connections</h2>
      <ConnectionsTable :connections="connections" />
    </template>
  </section>
</template>
