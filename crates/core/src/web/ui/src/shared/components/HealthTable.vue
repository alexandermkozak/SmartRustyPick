<script lang="ts" setup>
/**
 * A set of measures: what was measured, what it came to, and what to do.
 *
 * The verdict and the threshold sit beside the number on purpose. A dashboard
 * whose numbers nobody knows how to read is not information, and a verdict
 * without the rule behind it is something to be argued with rather than acted
 * on - so both are shown, and the threshold is the server's own wording of the
 * rule rather than this page's guess at it.
 */
import {computed} from 'vue'
import HealthPill from './HealthPill.vue'
import type {Health} from '@shared/health'

const props = withDefaults(defineProps<{health: Health | null; concernsOnly?: boolean}>(), {
  concernsOnly: false,
})

// Worst first, so the row that needs doing something about is the first one
// read. A stable sort keeps the server's order within a verdict.
const order = {act: 0, watch: 1, good: 2}
const rows = computed(() => {
  const measures = props.health?.measures ?? []
  const shown = props.concernsOnly
    ? measures.filter((measure) => measure.verdict !== 'good')
    : measures
  return [...shown].sort((a, b) => order[a.verdict] - order[b.verdict])
})
</script>

<template>
  <p v-if="!rows.length" class="empty">Nothing to report.</p>
  <ul v-else class="measures">
    <li v-for="measure in rows" :key="measure.id" :class="['measure', measure.verdict]">
      <div class="row">
        <span class="label">{{ measure.label }}</span>
        <span class="value mono">{{ measure.value }}</span>
        <HealthPill :verdict="measure.verdict" />
      </div>
      <p class="detail">{{ measure.detail }}</p>
      <p v-if="measure.verdict !== 'good'" class="threshold">{{ measure.threshold }}</p>
    </li>
  </ul>
</template>
