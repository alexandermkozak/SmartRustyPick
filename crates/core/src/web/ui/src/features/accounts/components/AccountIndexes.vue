<script lang="ts" setup>
/**
 * Every index in the account that is not earning its keep.
 *
 * The gap this closes is the first one an operator meets: nothing reported on
 * an index unless somebody opened the page for its file, so a database with
 * forty files had no view that said "these three are the ones worth your
 * attention" - and a problem nobody is told about is a problem nobody finds.
 * There are three columns of navigation before a single index otherwise.
 *
 * Only the indexes with something to say are listed. A table of every index in
 * the account would be the same wall of numbers one level further out; what is
 * wanted here is the exceptions, and the file's own table for the rest.
 */
import {computed} from 'vue'
import HealthPill from '@shared/components/HealthPill.vue'
import {count} from '@shared/format'
import {concerns, rollUp} from '@shared/health'
import type {IndexStats} from '../types'

const props = defineProps<{
  account: string | null
  indexes: IndexStats[]
  loaded: boolean
}>()
defineEmits<{open: [file: string]}>()

/** Worst first, then by file and field so the order is stable. */
const worrying = computed(() =>
  props.indexes
    .filter((index) => index.health.verdict !== 'good')
    .map((index) => ({index, why: concerns(index.health)}))
    .sort(
      (a, b) =>
        (b.index.health.verdict === 'act' ? 1 : 0) - (a.index.health.verdict === 'act' ? 1 : 0) ||
        a.index.file.localeCompare(b.index.file) ||
        a.index.field.localeCompare(b.index.field),
    ),
)

const verdict = computed(() => rollUp(props.indexes.map((index) => index.health.verdict)))
</script>

<template>
  <h2>Indexes in {{ account }}</h2>
  <div class="card">
    <p v-if="!loaded" class="empty">Loading…</p>
    <p v-else-if="!indexes.length" class="empty">
      No file in this account carries a secondary index, so every selection other than by key reads
      every record of the file it names.
    </p>
    <template v-else>
      <p class="health-summary">
        <HealthPill :verdict="verdict" show-good />
        <span>
          {{ count(indexes.length) }} indexes across
          {{ count(new Set(indexes.map((index) => index.file)).size) }} files
        </span>
        <span v-if="worrying.length">{{ count(worrying.length) }} need attention</span>
      </p>
      <p v-if="!worrying.length" class="note">
        Every one of them matches its records, narrows what it is asked for, and is being used.
      </p>
      <ul v-else class="list">
        <li v-for="row in worrying" :key="`${row.index.file}/${row.index.field}`" class="entry">
          <button class="select" type="button" @click="$emit('open', row.index.file)">
            <span class="row">
              <span class="mono">{{ row.index.file }}/{{ row.index.field }}</span>
              <HealthPill :verdict="row.index.health.verdict" />
            </span>
            <span v-for="measure in row.why" :key="measure.id" class="meta">
              {{ measure.label }}: {{ measure.value }} — {{ measure.detail }}
            </span>
          </button>
        </li>
      </ul>
    </template>
  </div>
</template>
