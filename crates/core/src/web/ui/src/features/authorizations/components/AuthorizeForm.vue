<script lang="ts" setup>
/** Authorizes a certificate that already exists, by its thumbprint. */
import {reactive, ref} from 'vue'
import CapabilityPicker from '@shared/components/CapabilityPicker.vue'
import {splitAccounts} from '../composables/useClients'
import type {AuthorizationRequest} from '../types'

const emit = defineEmits<{submit: [request: AuthorizationRequest]}>()

const form = reactive({
  name: '',
  thumbprint: '',
  accounts: '',
  is_admin: false,
  capabilities: [] as string[],
})
const submitting = ref(false)

async function submit(): Promise<void> {
  submitting.value = true
  emit('submit', {
    name: form.name.trim(),
    thumbprint: form.thumbprint.trim().toLowerCase(),
    accounts: splitAccounts(form.accounts),
    is_admin: form.is_admin,
    capabilities: form.capabilities,
  })
  submitting.value = false
}

/** Called by the parent once the server has accepted the authorization. */
function reset(): void {
  Object.assign(form, {name: '', thumbprint: '', accounts: '', is_admin: false, capabilities: []})
}

defineExpose({reset})
</script>

<template>
  <form class="card form" @submit.prevent="submit">
    <label>
      Name
      <input v-model="form.name" autocomplete="off" placeholder="reporting-service" required />
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
      <input v-model="form.accounts" autocomplete="off" placeholder="SALES, REPORTS" />
    </label>
    <label class="check">
      <input v-model="form.is_admin" type="checkbox" />
      Administrator (all accounts, every capability)
    </label>
    <CapabilityPicker v-model="form.capabilities" :disabled="form.is_admin" />
    <p v-if="!form.is_admin && !form.accounts.trim() && !form.capabilities.length" class="hint">
      A non-admin client needs at least one allowed account or capability.
    </p>
    <button :disabled="submitting" type="submit">
      {{ submitting ? 'Authorizing…' : 'Authorize' }}
    </button>
  </form>
</template>
