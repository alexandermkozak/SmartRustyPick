<script lang="ts" setup>
defineProps<{
  label: string
  value: string
  /** Draws attention when the number is one that should usually be zero. */
  tone?: 'neutral' | 'warn'
}>()
</script>

<template>
  <div :class="{'stat-warn': tone === 'warn'}" class="stat">
    <div class="value">{{ value }}</div>
    <div class="label">{{ label }}</div>
  </div>
</template>
