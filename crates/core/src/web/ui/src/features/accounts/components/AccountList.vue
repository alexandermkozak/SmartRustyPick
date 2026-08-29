<script lang="ts" setup>
/** Every account, with the figures that say how big it is. */
import PanelState from '@shared/components/PanelState.vue'
import {bytes, count} from '@shared/format'
import type {AccountStats} from '../types'

defineProps<{
  accounts: AccountStats[]
  loaded: boolean
  error: string | null
  selected: string | null
}>()
defineEmits<{select: [name: string]}>()
</script>

<template>
  <h2>Accounts</h2>
  <PanelState :empty="!accounts.length" :error="error" :loaded="loaded" empty-text="No accounts." />
  <ul v-if="accounts.length" class="list">
    <li v-for="account in accounts" :key="account.name">
      <button
        :aria-current="selected === account.name"
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
    </li>
  </ul>
</template>
