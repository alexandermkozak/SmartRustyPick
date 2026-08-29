<script lang="ts" setup>
/**
 * The header's one-line readout of what the dashboard is attached to. Placed by
 * the shell, but owned here: it reads the same snapshot the overview does.
 */
import {computed} from 'vue'
import {useServerStats} from '../composables/useServerStats'
import {duration} from '@shared/format'

const stats = useServerStats()

const line = computed(() => {
  const snapshot = stats.data.value
  if (!snapshot) return stats.error.value ? 'not responding' : 'connecting…'
  return `${snapshot.listen_addr} · up ${duration(snapshot.uptime_seconds)}`
})
</script>

<template>
  <p class="sub">{{ line }}</p>
</template>
