<script lang="ts" setup>
import {reactive, ref} from 'vue'
import RolePill from '../components/RolePill.vue'
import PanelState from '../components/PanelState.vue'
import {api} from '../api'
import {useAlerts} from '../composables/useAlerts'
import {usePolling} from '../composables/usePolling'
import {shortThumbprint} from '../format'
import type {ClientEntry} from '../types'

// The authorization list changes only when someone changes it - here, in a CLI
// beside the server, or in another dashboard - so it is polled slowly rather
// than not at all.
const {data, error, loaded, refresh} = usePolling<ClientEntry[]>(api.clients, {intervalMs: 15000})
const alerts = useAlerts()

const form = reactive({name: '', thumbprint: '', accounts: '', is_admin: false})
const submitting = ref(false)

/** Which row has its account editor open, and what has been typed into it. */
const editing = ref<string | null>(null)
const accountDraft = ref('')

const splitAccounts = (value: string): string[] =>
    value
        .split(',')
        .map((account) => account.trim())
        .filter(Boolean)

async function authorize(): Promise<void> {
  submitting.value = true
  const ok = await alerts.attempt(() =>
      api.authorize({
        name: form.name.trim(),
        thumbprint: form.thumbprint.trim().toLowerCase(),
        accounts: splitAccounts(form.accounts),
        is_admin: form.is_admin,
      }),
  )
  submitting.value = false
  if (ok) {
    Object.assign(form, {name: '', thumbprint: '', accounts: '', is_admin: false})
    await refresh()
  }
}

function openEditor(name: string): void {
  editing.value = editing.value === name ? null : name
  accountDraft.value = ''
}

async function changeAccounts(name: string, remove: boolean): Promise<void> {
  const accounts = splitAccounts(accountDraft.value)
  if (!accounts.length) return
  if (await alerts.attempt(() => api.changeAccounts(name, accounts, remove))) {
    accountDraft.value = ''
    await refresh()
  }
}

async function revoke(name: string): Promise<void> {
  if (!window.confirm(`Revoke "${name}"? Its certificate stops working immediately.`)) return
  if (await alerts.attempt(() => api.revoke(name))) {
    if (editing.value === name) editing.value = null
    await refresh()
  }
}
</script>

<template>
  <section>
    <h2>Authorized clients</h2>
    <PanelState
        :empty="!(data ?? []).length"
        :error="error"
        :loaded="loaded"
        empty-text="No authorized clients."
    />

    <div v-if="loaded && (data ?? []).length" class="table-wrap">
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
        <template v-for="client in data ?? []" :key="client.name">
          <tr>
            <td>{{ client.name }}</td>
            <td :title="client.info.thumbprint" class="mono">
              {{ shortThumbprint(client.info.thumbprint) }}
            </td>
            <td>
              {{ client.info.accounts.join(', ') || (client.info.is_admin ? 'all' : '—') }}
            </td>
            <td>
              <RolePill :is-admin="client.info.is_admin"/>
            </td>
            <td class="actions">
              <button class="small" type="button" @click="openEditor(client.name)">
                {{ editing === client.name ? 'Close' : 'Accounts' }}
              </button>
              <button class="small danger" type="button" @click="revoke(client.name)">
                Revoke
              </button>
            </td>
          </tr>
          <tr v-if="editing === client.name" class="editor-row">
            <td colspan="5">
              <form class="inline-form" @submit.prevent="changeAccounts(client.name, false)">
                <input
                    v-model="accountDraft"
                    aria-label="Accounts to add or remove"
                    autocomplete="off"
                    placeholder="SALES, REPORTS"
                />
                <button :disabled="!accountDraft.trim()" class="small" type="submit">Add</button>
                <button
                    :disabled="!accountDraft.trim()"
                    class="small"
                    type="button"
                    @click="changeAccounts(client.name, true)"
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

    <h2>Authorize an existing certificate</h2>
    <form class="card form" @submit.prevent="authorize">
      <label>
        Name
        <input v-model="form.name" autocomplete="off" placeholder="reporting-service" required/>
      </label>
      <label>
        SHA-256 thumbprint
        <input
            v-model="form.thumbprint"
            autocomplete="off"
            pattern="[0-9a-fA-F]{16,128}"
            placeholder="lowercase hex"
            required
            title="The certificate's SHA-256 thumbprint, in hex"
        />
      </label>
      <label>
        Allowed accounts
        <input v-model="form.accounts" autocomplete="off" placeholder="SALES, REPORTS"/>
      </label>
      <label class="check">
        <input v-model="form.is_admin" type="checkbox"/>
        Administrator (all accounts, management commands)
      </label>
      <p v-if="!form.is_admin && !form.accounts.trim()" class="hint">
        A non-admin client needs at least one allowed account.
      </p>
      <button :disabled="submitting" type="submit">
        {{ submitting ? 'Authorizing…' : 'Authorize' }}
      </button>
    </form>
  </section>
</template>
