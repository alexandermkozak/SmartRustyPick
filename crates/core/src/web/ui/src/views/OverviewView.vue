<script lang="ts" setup>
import {computed} from 'vue'
import StatCard from '../components/StatCard.vue'
import RolePill from '../components/RolePill.vue'
import {useServerStats} from '../composables/useServerStats'
import {count, duration, timestamp} from '../format'

const {data, error, loaded} = useServerStats()

const connections = computed(() => data.value?.active_connections ?? [])

const cards = computed(() => {
  const stats = data.value
  if (!stats) return []
  return [
    {label: 'Active connections', value: count(connections.value.length)},
    {label: 'Connections served', value: count(stats.total_connections)},
    {label: 'Rejected', value: count(stats.rejected_connections), warnAbove: 0},
    {label: 'Requests', value: count(stats.total_requests)},
    {label: 'Failed requests', value: count(stats.failed_requests), warnAbove: 0},
    {label: 'Pending writes', value: count(stats.pending_writes)},
    {label: 'Tables in memory', value: count(stats.loaded_tables)},
    {label: 'Authorized clients', value: count(stats.authorized_clients)},
  ]
})

const raw = computed(() => data.value)

const tone = (card: { value: string; warnAbove?: number }) =>
    card.warnAbove !== undefined && card.value !== '0' ? 'warn' : 'neutral'
</script>

<template>
  <section>
    <!-- A failed refresh dims the numbers instead of removing them: stale data
         with a visible reason beats an empty screen. -->
    <p v-if="loaded && error" class="alert inline">{{ error }} — showing the last known values.</p>

    <p v-if="!loaded && error" class="empty error-text">{{ error }}</p>
    <p v-else-if="!loaded" class="empty">Loading…</p>

    <template v-else>
      <div :class="{ stale: !!error }" class="stat-grid">
        <StatCard
            v-for="card in cards"
            :key="card.label"
            :label="card.label"
            :tone="tone(card)"
            :value="card.value"
        />
      </div>

      <h2>Server</h2>
      <div class="card">
        <dl class="stats">
          <dt>Protocol listener</dt>
          <dd class="mono">{{ raw?.listen_addr }}</dd>
          <dt>Started</dt>
          <dd>{{ timestamp(raw?.started_at) }}</dd>
          <dt>Uptime</dt>
          <dd>{{ duration(raw?.uptime_seconds) }}</dd>
        </dl>
      </div>

      <h2>Active connections</h2>
      <div class="table-wrap">
        <table>
          <thead>
          <tr>
            <th>Client</th>
            <th>Peer</th>
            <th>Role</th>
            <th class="num">Connected</th>
            <th class="num">Requests</th>
            <th>Last command</th>
            <th class="num">Idle</th>
          </tr>
          </thead>
          <tbody>
          <tr v-for="connection in connections" :key="connection.id">
            <td>{{ connection.client_name || '—' }}</td>
            <td class="mono">{{ connection.peer }}</td>
            <td>
              <RolePill :is-admin="connection.is_admin"/>
            </td>
            <td class="num">{{ duration(connection.connected_seconds) }}</td>
            <td class="num">{{ count(connection.requests) }}</td>
            <td>{{ connection.last_command || '—' }}</td>
            <td class="num">{{ duration(connection.idle_seconds) }}</td>
          </tr>
          <tr v-if="!connections.length">
            <td class="empty" colspan="7">No open connections.</td>
          </tr>
          </tbody>
        </table>
      </div>
    </template>
  </section>
</template>
