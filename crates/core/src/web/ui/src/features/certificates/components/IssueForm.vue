<script lang="ts" setup>
/** Names a certificate and the accounts it may reach. */
import {reactive} from 'vue'
import type {CertificateRequest} from '../types'

defineProps<{issuing: boolean}>()
const emit = defineEmits<{submit: [request: CertificateRequest]}>()

const form = reactive({common_name: '', accounts: '', is_admin: false})

function submit(): void {
  emit('submit', {
    common_name: form.common_name.trim(),
    accounts: form.accounts
      .split(',')
      .map((account) => account.trim())
      .filter(Boolean),
    is_admin: form.is_admin,
  })
}

function reset(): void {
  Object.assign(form, {common_name: '', accounts: '', is_admin: false})
}

defineExpose({reset})
</script>

<template>
  <form class="card form" @submit.prevent="submit">
    <label>
      Common name
      <input
        v-model="form.common_name"
        autocomplete="off"
        pattern="[A-Za-z0-9._-]+"
        placeholder="reporting-service"
        required
        title="Letters, digits, dot, dash and underscore only"
      />
    </label>
    <label>
      Allowed accounts
      <input v-model="form.accounts" autocomplete="off" placeholder="SALES, REPORTS" />
    </label>
    <label class="check">
      <input v-model="form.is_admin" type="checkbox" />
      Administrator certificate
    </label>
    <p v-if="!form.is_admin && !form.accounts.trim()" class="hint">
      A non-admin certificate needs at least one allowed account.
    </p>
    <button :disabled="issuing" type="submit">
      {{ issuing ? 'Generating…' : 'Generate and authorize' }}
    </button>
  </form>
</template>
