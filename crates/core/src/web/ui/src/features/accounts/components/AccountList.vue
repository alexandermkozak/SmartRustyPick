<script lang="ts" setup>
/**
 * Every account, with the figures that say how big it is, whether anything in
 * it needs attention, and the two things an operator does to the list itself:
 * add one, drop one.
 *
 * The verdict is the worst of the account's files, so a database with forty
 * files does not need anyone to remember to go and look at each of them.
 */
import {ref} from 'vue'
import HealthPill from '@shared/components/HealthPill.vue'
import PanelState from '@shared/components/PanelState.vue'
import {bytes, count} from '@shared/format'
import {verdictOf} from '@shared/health'
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
  create: [name: string, demo: boolean]
  drop: [name: string]
}>()

const draft = ref('')

/**
 * One name, two kinds of account. A demo account is the `CREATE.TEST.ACCOUNT`
 * fixture the CLI makes: two files with dictionaries and a few records in them,
 * which is enough to try a query or the dictionary editor against.
 */
function create(demo: boolean): void {
  const name = draft.value.trim()
  if (!name) return
  draft.value = ''
  emit('create', name, demo)
}

// Dropping an account takes every file in it, and nothing here can put them
// back, so the confirmation says what actually goes.
/** What the roll-up says, when there is anything to say. */
function concern(account: AccountStats): string {
  const parts: string[] = []
  if (account.unhealthy_files) parts.push(`${count(account.unhealthy_files)} files need attention`)
  if (account.stale_indexes) parts.push(`${count(account.stale_indexes)} stale indexes`)
  return parts.join(' · ')
}

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
        <span class="row">
          <span>{{ account.name }}</span>
          <HealthPill :title="concern(account)" :verdict="verdictOf(account.health?.verdict)" />
        </span>
        <span class="meta">
          {{ count(account.file_count) }} files · {{ count(account.record_count) }} records ·
          {{ bytes(account.disk_bytes) }}
          <template v-if="account.index_count">
            · {{ count(account.index_count) }} indexes
          </template>
        </span>
        <span v-if="concern(account)" class="meta">{{ concern(account) }}</span>
        <span class="meta">{{ account.directory }}</span>
      </button>
      <button :disabled="busy" class="small danger" type="button" @click="drop(account)">
        Drop
      </button>
    </li>
  </ul>

  <form class="inline-form spaced new-account" @submit.prevent="create(false)">
    <input v-model="draft" aria-label="New account name" autocomplete="off" placeholder="SALES" />
    <button :disabled="busy || !draft.trim()" class="small" type="submit">Create account</button>
    <button
      :disabled="busy || !draft.trim()"
      class="small"
      title="Creates the account populated with the CLI's CREATE.TEST.ACCOUNT fixture"
      type="button"
      @click="create(true)"
    >
      Create demo
    </button>
  </form>
  <p class="note small-note">
    A demo account arrives with <span class="mono">USERS</span> and
    <span class="mono">PRODUCTS</span> — dictionaries, a multivalued field and a priced item — so
    there is something to query and something to edit.
  </p>
</template>
