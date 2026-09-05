<script lang="ts" setup>
/**
 * The files of the selected account, each with what it is and its health at a
 * glance, and the two things an operator does to the list itself.
 *
 * The health pill is why this list is worth reading rather than only clicking
 * through: a file whose format is out of date or whose index has fallen behind
 * says so here, so a problem is findable without opening every file in turn.
 * It is the cheap verdict - metadata only - and the panel to the right has the
 * measures behind it.
 */
import {reactive} from 'vue'
import HealthPill from '@shared/components/HealthPill.vue'
import type {FileEntry, QueueDraft} from '../types'

const props = defineProps<{
  account: string | null
  files: FileEntry[]
  loaded: boolean
  selected: string | null
  busy: boolean
}>()
const emit = defineEmits<{
  select: [file: string]
  create: [name: string, durable: boolean, queue: QueueDraft | null]
  drop: [name: string]
}>()

const draft = reactive({name: '', durable: false, queue: false, timeout: '', retries: ''})

/**
 * The claim policy the form asks for, or nothing when it is not a queue.
 *
 * A blank timeout or retry count is left out rather than sent as a zero: the
 * database has defaults for both, and this page is not the place they are
 * decided.
 */
function queueDraft(): QueueDraft | null {
  if (!draft.queue) return null
  const policy: QueueDraft = {}
  const timeout = Number(draft.timeout)
  const retries = Number(draft.retries)
  if (draft.timeout.trim() && Number.isFinite(timeout)) policy.visibility_timeout = timeout
  if (draft.retries.trim() && Number.isFinite(retries)) policy.max_deliveries = retries
  return policy
}

function create(): void {
  const name = draft.name.trim()
  if (!name) return
  emit('create', name, draft.durable, queueDraft())
  Object.assign(draft, {name: '', durable: false, queue: false, timeout: '', retries: ''})
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
              v-if="file.queue"
              class="tag queue"
              title="Ordered records, handed to one consumer at a time"
            >
              queue
            </span>
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
    <label class="check">
      <input v-model="draft.queue" type="checkbox" />
      Queue
    </label>
    <button :disabled="busy || !draft.name.trim()" class="small" type="submit">Create file</button>
  </form>
  <!-- Only once the file is to be a queue: the two numbers mean nothing on an
       ordinary file, and a form that always showed them would suggest they do. -->
  <form v-if="account && draft.queue" class="inline-form spaced new-file" @submit.prevent="create">
    <input
      v-model="draft.timeout"
      aria-label="Visibility timeout in seconds"
      inputmode="numeric"
      placeholder="timeout 60s"
    />
    <input
      v-model="draft.retries"
      aria-label="Deliveries before dead-lettering"
      inputmode="numeric"
      placeholder="retries 5"
    />
    <span class="note">A queue is durable unless you say otherwise.</span>
  </form>
</template>
