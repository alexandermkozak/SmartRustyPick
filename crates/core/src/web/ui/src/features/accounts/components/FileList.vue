<script lang="ts" setup>
/**
 * The files of the selected account, each with its durability and its health at
 * a glance, and the two things an operator does to the list itself.
 *
 * The health pill is why this list is worth reading rather than only clicking
 * through: a file whose format is out of date or whose index has fallen behind
 * says so here, so a problem is findable without opening every file in turn.
 * It is the cheap verdict - metadata only - and the panel to the right has the
 * measures behind it.
 */
import {reactive} from 'vue'
import HealthPill from '@shared/components/HealthPill.vue'
import type {FileEntry} from '../types'

const props = defineProps<{
  account: string | null
  files: FileEntry[]
  loaded: boolean
  selected: string | null
  busy: boolean
}>()
const emit = defineEmits<{
  select: [file: string]
  create: [name: string, durable: boolean]
  drop: [name: string]
}>()

const draft = reactive({name: '', durable: false})

function create(): void {
  const name = draft.name.trim()
  if (!name) return
  emit('create', name, draft.durable)
  Object.assign(draft, {name: '', durable: false})
}

/**
 * DIR is the account's own record of its files and their durability flags.
 * Dropping it would take the flags with it, so it is listed like any other file
 * and is the one the page will not offer to remove.
 */
const droppable = (file: FileEntry): boolean => file.name !== 'DIR'

function drop(name: string): void {
  const where = props.account ? ` from ${props.account}` : ''
  if (!window.confirm(`Drop "${name}"${where}? Its records and dictionary go with it.`)) return
  emit('drop', name)
}
</script>

<template>
  <h2>{{ account ? `Files in ${account}` : 'Files' }}</h2>
  <ul class="list">
    <li v-if="!account" class="empty">Select an account.</li>
    <li v-else-if="!loaded" class="empty">Loading…</li>
    <li v-else-if="!files.length" class="empty">No files in this account.</li>
    <li v-for="file in files" v-else :key="file.name" class="entry">
      <button
        :aria-current="selected === file.name"
        class="select"
        type="button"
        @click="$emit('select', file.name)"
      >
        <span class="row">
          <span>{{ file.name }}</span>
          <span class="tags">
            <HealthPill :title="file.health.reasons.join('; ')" :verdict="file.health.verdict" />
            <span
              v-if="file.durable"
              class="tag durable"
              title="Every write is flushed before it is acknowledged"
            >
              durable
            </span>
          </span>
        </span>
      </button>
      <button
        v-if="droppable(file)"
        :disabled="busy"
        class="small danger"
        type="button"
        @click="drop(file.name)"
      >
        Drop
      </button>
    </li>
  </ul>

  <form v-if="account" class="inline-form spaced new-file" @submit.prevent="create">
    <input
      v-model="draft.name"
      aria-label="New file name"
      autocomplete="off"
      placeholder="LEDGER"
    />
    <label class="check">
      <input v-model="draft.durable" type="checkbox" />
      Durable
    </label>
    <button :disabled="busy || !draft.name.trim()" class="small" type="submit">Create file</button>
  </form>
</template>
