<script lang="ts" setup>
import {computed, ref, watch} from 'vue'
import PanelState from '../components/PanelState.vue'
import StatList from '../components/StatList.vue'
import {api} from '../api'
import {useAlerts} from '../composables/useAlerts'
import {usePolling} from '../composables/usePolling'
import {bytes, count, duration} from '../format'
import type {AccountStats, FileStats} from '../types'

// Accounts and files change when someone creates or drops one, so a slow poll
// keeps the list honest without making navigation feel busy.
const accounts = usePolling<AccountStats[]>(api.accounts, {intervalMs: 20000})
const alerts = useAlerts()

const selectedAccount = ref<string | null>(null)
const selectedFile = ref<string | null>(null)
const files = ref<string[]>([])
const filesLoaded = ref(false)
const stats = ref<FileStats | null>(null)

async function selectAccount(name: string): Promise<void> {
  selectedAccount.value = name
  selectedFile.value = null
  stats.value = null
  files.value = []
  filesLoaded.value = false
  try {
    files.value = await api.files(name)
    alerts.clear()
  } catch (cause) {
    alerts.fail(cause)
  } finally {
    filesLoaded.value = true
  }
}

async function selectFile(file: string): Promise<void> {
  if (!selectedAccount.value) return
  selectedFile.value = file
  try {
    stats.value = await api.fileStats(selectedAccount.value, file)
    alerts.clear()
  } catch (cause) {
    stats.value = null
    alerts.fail(cause)
  }
}

// An account dropped by someone else should not leave a dead selection behind.
watch(accounts.data, (list) => {
  if (!list || !selectedAccount.value) return
  if (!list.some((account) => account.name === selectedAccount.value)) {
    selectedAccount.value = null
    selectedFile.value = null
    files.value = []
    filesLoaded.value = false
    stats.value = null
  }
})

const fileRows = computed<Array<[string, string]>>(() => {
  const file = stats.value
  if (!file) return []
  return [
    ['Records', count(file.record_count)],
    ['Dictionary entries', count(file.dict_count)],
    ['Hash modulus', count(file.modulus)],
    ['Group files', count(file.group_count)],
    ['Smallest group', bytes(file.smallest_group_bytes)],
    ['Largest group', bytes(file.largest_group_bytes)],
    ['On disk', bytes(file.disk_bytes)],
    ['Flush version', count(file.version)],
    ['Durable writes', file.durable ? 'yes' : 'no'],
    ['Checksums', file.checksums ? 'yes' : 'no'],
    ['Format', file.legacy ? 'legacy flat file' : 'hashed'],
    ['In memory', file.loaded ? 'yes' : 'no'],
    [
      'Last modified',
      file.modified_seconds_ago === null ? '—' : `${duration(file.modified_seconds_ago)} ago`,
    ],
  ]
})
</script>

<template>
  <section class="split">
    <div>
      <h2>Accounts</h2>
      <PanelState
          :empty="!(accounts.data.value ?? []).length"
          :error="accounts.error.value"
          :loaded="accounts.loaded.value"
          empty-text="No accounts."
      />
      <ul v-if="(accounts.data.value ?? []).length" class="list">
        <li v-for="account in accounts.data.value ?? []" :key="account.name">
          <button
              :aria-current="selectedAccount === account.name"
              type="button"
              @click="selectAccount(account.name)"
          >
            {{ account.name }}
            <span class="meta">
              {{ count(account.file_count) }} files · {{ count(account.record_count) }} records ·
              {{ bytes(account.disk_bytes) }}
            </span>
            <span class="meta">{{ account.directory }}</span>
          </button>
        </li>
      </ul>
    </div>

    <div>
      <h2>{{ selectedAccount ? `Files in ${selectedAccount}` : 'Files' }}</h2>
      <ul class="list">
        <li v-if="!selectedAccount" class="empty">Select an account.</li>
        <li v-else-if="!filesLoaded" class="empty">Loading…</li>
        <li v-else-if="!files.length" class="empty">No files in this account.</li>
        <li v-for="file in files" v-else :key="file">
          <button :aria-current="selectedFile === file" type="button" @click="selectFile(file)">
            {{ file }}
          </button>
        </li>
      </ul>
    </div>

    <div>
      <h2>File statistics</h2>
      <div class="card">
        <p v-if="!stats" class="empty">Select a file.</p>
        <template v-else>
          <h3 class="mono">{{ stats.account }}/{{ stats.name }}</h3>
          <StatList :rows="fileRows"/>
        </template>
      </div>
    </div>
  </section>
</template>
