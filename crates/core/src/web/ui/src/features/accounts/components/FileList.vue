<script lang="ts" setup>
/** The files of the selected account. */
defineProps<{
  account: string | null
  files: string[]
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
    <li v-for="file in files" v-else :key="file">
      <button :aria-current="selected === file" type="button" @click="$emit('select', file)">
        {{ file }}
      </button>
    </li>
  </ul>
</template>
