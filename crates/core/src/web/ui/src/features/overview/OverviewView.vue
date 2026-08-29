<script lang="ts" setup>
import {computed} from 'vue'
import StatGrid from './components/StatGrid.vue'
import ConnectionsTable from './components/ConnectionsTable.vue'
import {useServerStats} from './composables/useServerStats'
import {duration, timestamp} from '@shared/format'

const {data, error, loaded} = useServerStats()

const connections = computed(() => data.value?.active_connections ?? [])
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

      <h2>Active connections</h2>
      <ConnectionsTable :connections="connections" />
    </template>
  </section>
</template>
