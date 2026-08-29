<script lang="ts" setup>
/**
 * The authorized clients, with the per-row account editor.
 *
 * Editing state is local to the table; changing anything is the parent's job,
 * so this stays a rendering concern and the reload rule stays in one place.
 */
import {ref} from 'vue'
import RolePill from '@shared/components/RolePill.vue'
import {shortThumbprint} from '@shared/format'
import {splitAccounts} from '../composables/useClients'
import type {ClientEntry} from '../types'

defineProps<{clients: ClientEntry[]}>()
const emit = defineEmits<{
  changeAccounts: [name: string, accounts: string[], remove: boolean]
  revoke: [name: string]
}>()

/** Which row has its account editor open, and what has been typed into it. */
const editing = ref<string | null>(null)
const draft = ref('')

function toggleEditor(name: string): void {
  editing.value = editing.value === name ? null : name
  draft.value = ''
}

function change(name: string, remove: boolean): void {
  const accounts = splitAccounts(draft.value)
  if (!accounts.length) return
  draft.value = ''
  emit('changeAccounts', name, accounts, remove)
}

function revoke(name: string): void {
  if (!window.confirm(`Revoke "${name}"? Its certificate stops working immediately.`)) return
  if (editing.value === name) editing.value = null
  emit('revoke', name)
}
</script>

<template>
  <div class="table-wrap">
    <table>
      <thead>
        <tr>
          <th>Name</th>
          <th>Thumbprint</th>
          <th>Accounts</th>
          <th>Role</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        <template v-for="client in clients" :key="client.name">
          <tr>
            <td>{{ client.name }}</td>
            <td :title="client.info.thumbprint" class="mono">
              {{ shortThumbprint(client.info.thumbprint) }}
            </td>
            <td>{{ client.info.accounts.join(', ') || (client.info.is_admin ? 'all' : '—') }}</td>
            <td>
              <RolePill :is-admin="client.info.is_admin" />
            </td>
            <td class="actions">
              <button class="small" type="button" @click="toggleEditor(client.name)">
                {{ editing === client.name ? 'Close' : 'Accounts' }}
              </button>
              <button class="small danger" type="button" @click="revoke(client.name)">
                Revoke
              </button>
            </td>
          </tr>
          <tr v-if="editing === client.name" class="editor-row">
            <td colspan="5">
              <form class="inline-form" @submit.prevent="change(client.name, false)">
                <input
                  v-model="draft"
                  aria-label="Accounts to add or remove"
                  autocomplete="off"
                  placeholder="SALES, REPORTS"
                />
                <button :disabled="!draft.trim()" class="small" type="submit">Add</button>
                <button
                  :disabled="!draft.trim()"
                  class="small"
                  type="button"
                  @click="change(client.name, true)"
                >
                  Remove
                </button>
              </form>
            </td>
          </tr>
        </template>
      </tbody>
    </table>
  </div>
</template>
