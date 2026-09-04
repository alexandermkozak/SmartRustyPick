<script lang="ts" setup>
/**
 * A verdict as a coloured pill. The one thing a listing has room for, and the
 * thing that makes a problem findable without opening every file in turn.
 */
import {verdictLabel, type Verdict} from '@shared/health'

const props = withDefaults(defineProps<{verdict: Verdict; title?: string; showGood?: boolean}>(), {
  title: '',
  showGood: false,
})
</script>

<template>
  <span
    v-if="props.showGood || props.verdict !== 'good'"
    :class="['tag', 'verdict', props.verdict]"
    :title="props.title || verdictLabel(props.verdict)"
  >
    {{ verdictLabel(props.verdict) }}
  </span>
</template>
