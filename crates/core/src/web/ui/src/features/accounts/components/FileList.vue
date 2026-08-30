<script lang="ts" setup>
/** The files of the selected account, each with its durability at a glance. */
import type {FileEntry} from '../types'

defineProps<{
  account: string | null
  files: FileEntry[]
  loaded: boolean
  selected: string | null
}>()
defineEmits<{select: [file: string]}>()
</script>

<template>
  <h2>{{ account ? `Files in ${account}` : 'Files' }}</h2>
  <ul class="list">
    <li v-if="!account" class="empty">Select an account.</li>
    <li v-else-if="!loaded" class="empty">Loading…</li>
    <li v-else-if="!files.length" class="empty">No files in this account.</li>
    <li v-for="file in files" v-else :key="file.name">
      <button
        :aria-current="selected === file.name"
        type="button"
        @click="$emit('select', file.name)"
      >
        <span class="row">
          <span>{{ file.name }}</span>
          <span
            v-if="file.durable"
            class="tag durable"
            title="Every write is flushed before it is acknowledged"
          >
            durable
          </span>
        </span>
      </button>
    </li>
  </ul>
</template>
