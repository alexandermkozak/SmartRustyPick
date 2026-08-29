<script lang="ts" setup>
/**
 * The header's connection indicator and the controls for the live poll: pause
 * it, or ask for a refresh now. Owned by this slice because it drives the same
 * poller the overview reads.
 */
import {computed} from 'vue'
import {useServerStats} from '../composables/useServerStats'

const stats = useServerStats()

const health = computed(() => {
  if (stats.error.value) return {text: stats.live.value ? 'error' : 'disconnected', tone: 'down'}
  if (!stats.loaded.value) return {text: '…', tone: ''}
  return {text: 'connected', tone: 'up'}
})

function toggleLive(): void {
  if (stats.live.value) stats.stop()
  else stats.start()
}
</script>

<template>
  <span :class="health.tone" class="pill">{{ health.text }}</span>
  <label class="check live-toggle">
    <input :checked="stats.live.value" type="checkbox" @change="toggleLive" />
    Live
  </label>
  <button :disabled="stats.loading.value" class="ghost" type="button" @click="stats.refresh()">
    Refresh
  </button>
</template>
