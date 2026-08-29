<script lang="ts" setup>
/** The overview's headline figures, dimmed while a refresh is failing. */
import {computed} from 'vue'
import StatCard from '@shared/components/StatCard.vue'
import {count} from '@shared/format'
import type {ServerSnapshot} from '../types'

const props = defineProps<{snapshot: ServerSnapshot; stale: boolean}>()

// `warn` marks the figures that should normally read zero, so a non-zero one is
// noticed without having to be hunted for.
const cards = computed(() => [
  {label: 'Active connections', value: count(props.snapshot.active_connections.length)},
  {label: 'Connections served', value: count(props.snapshot.total_connections)},
  {label: 'Rejected', value: count(props.snapshot.rejected_connections), warnWhenNotZero: true},
  {label: 'Requests', value: count(props.snapshot.total_requests)},
  {label: 'Failed requests', value: count(props.snapshot.failed_requests), warnWhenNotZero: true},
  {label: 'Pending writes', value: count(props.snapshot.pending_writes)},
  {label: 'Tables in memory', value: count(props.snapshot.loaded_tables)},
  {label: 'Authorized clients', value: count(props.snapshot.authorized_clients)},
])
</script>

<template>
  <div :class="{stale}" class="stat-grid">
    <StatCard
      v-for="card in cards"
      :key="card.label"
      :label="card.label"
      :tone="card.warnWhenNotZero && card.value !== '0' ? 'warn' : 'neutral'"
      :value="card.value"
    />
  </div>
</template>
