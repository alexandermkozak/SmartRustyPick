<script lang="ts" setup>
/** Every account, with the figures that say how big it is, and the two things
 *  an operator does to the list itself: add one, drop one. */
import {ref} from 'vue'
import PanelState from '@shared/components/PanelState.vue'
import {bytes, count} from '@shared/format'
import type {AccountStats} from '../types'

defineProps<{
  accounts: AccountStats[]
  loaded: boolean
  error: string | null
  selected: string | null
  busy: boolean
}>()
const emit = defineEmits<{
  select: [name: string]
  create: [name: string]
  drop: [name: string]
}>()

const draft = ref('')

function create(): void {
  const name = draft.value.trim()
  if (!name) return
  draft.value = ''
  emit('create', name)
}

// Dropping an account takes every file in it, and nothing here can put them
// back, so the confirmation says what actually goes.
function drop(account: AccountStats): void {
  const files = `${count(account.file_count)} file${account.file_count === 1 ? '' : 's'}`
  if (!window.confirm(`Drop "${account.name}" and its ${files}? This cannot be undone.`)) return
  emit('drop', account.name)
}
</script>

<template>
  <h2>Accounts</h2>
  <PanelState :empty="!accounts.length" :error="error" :loaded="loaded" empty-text="No accounts." />
  <ul v-if="accounts.length" class="list">
    <li v-for="account in accounts" :key="account.name" class="entry">
      <button
        :aria-current="selected === account.name"
        class="select"
        type="button"
        @click="$emit('select', account.name)"
      >
        {{ account.name }}
        <span class="meta">
          {{ count(account.file_count) }} files · {{ count(account.record_count) }} records ·
          {{ bytes(account.disk_bytes) }}
        </span>
        <span class="meta">{{ account.directory }}</span>
      </button>
      <button :disabled="busy" class="small danger" type="button" @click="drop(account)">
        Drop
      </button>
    </li>
  </ul>

  <form class="inline-form spaced new-account" @submit.prevent="create">
    <input v-model="draft" aria-label="New account name" autocomplete="off" placeholder="SALES" />
    <button :disabled="busy || !draft.trim()" class="small" type="submit">Create account</button>
  </form>
</template>
