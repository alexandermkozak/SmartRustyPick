<script lang="ts" setup>
/** Names a certificate and the accounts it may reach. */
import {reactive} from 'vue'
import CapabilityPicker from '@shared/components/CapabilityPicker.vue'
import type {CertificateRequest} from '../types'

defineProps<{issuing: boolean}>()
const emit = defineEmits<{submit: [request: CertificateRequest]}>()

const form = reactive({
  common_name: '',
  accounts: '',
  is_admin: false,
  capabilities: [] as string[],
  days: '',
})

function submit(): void {
  emit('submit', {
    common_name: form.common_name.trim(),
    accounts: form.accounts
      .split(',')
      .map((account) => account.trim())
      .filter(Boolean),
    is_admin: form.is_admin,
    capabilities: form.capabilities,
    // Left out entirely when blank, so the server's default applies rather than
    // this form having to know what it is.
    ...(form.days.trim() ? {days: Number(form.days)} : {}),
  })
}

function reset(): void {
  Object.assign(form, {common_name: '', accounts: '', is_admin: false, capabilities: [], days: ''})
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
    <CapabilityPicker v-model="form.capabilities" :disabled="form.is_admin" />
    <label>
      Lifetime in days
      <input
        v-model="form.days"
        autocomplete="off"
        min="1"
        placeholder="365 (the server's default)"
        step="1"
        type="number"
      />
    </label>
    <p class="hint">
      There is no revocation list, so a short lifetime is the withdrawal that happens whether or not
      anyone notices a leak. A value above the deployment's ceiling is refused, not shortened.
    </p>
    <p v-if="!form.is_admin && !form.accounts.trim() && !form.capabilities.length" class="hint">
      A non-admin certificate needs at least one allowed account or capability.
    </p>
    <button :disabled="issuing" type="submit">
      {{ issuing ? 'Generating…' : 'Generate and authorize' }}
    </button>
  </form>
</template>
