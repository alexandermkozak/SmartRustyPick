<script lang="ts" setup>
/**
 * The three states every polled panel has before it has rows to show: loading
 * for the first time, failed with nothing to fall back on, or loaded and empty.
 * Once there is data the panel renders its own content and this stays out of
 * the way, even while a later refresh is failing - that error belongs beside
 * the data, not instead of it.
 */
defineProps<{
  loaded: boolean
  error: string | null
  empty: boolean
  emptyText: string
}>()
</script>

<template>
  <p v-if="!loaded && error" class="empty error-text">{{ error }}</p>
  <p v-else-if="!loaded" class="empty">Loading…</p>
  <p v-else-if="empty" class="empty">{{ emptyText }}</p>
</template>
