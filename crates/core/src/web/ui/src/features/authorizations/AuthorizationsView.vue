<script lang="ts" setup>
import {computed, ref} from 'vue'
import PanelState from '@shared/components/PanelState.vue'
import ClientTable from './components/ClientTable.vue'
import AuthorizeForm from './components/AuthorizeForm.vue'
import {useClients} from './composables/useClients'
import type {AuthorizationRequest} from './types'

const {clients, authorize, changeAccounts, revoke} = useClients()

const form = ref<InstanceType<typeof AuthorizeForm> | null>(null)
const rows = computed(() => clients.data.value ?? [])

async function submit(request: AuthorizationRequest): Promise<void> {
  if (await authorize(request)) form.value?.reset()
}
</script>

<template>
  <section>
    <h2>Authorized clients</h2>
    <PanelState
      :empty="!rows.length"
      :error="clients.error.value"
      :loaded="clients.loaded.value"
      empty-text="No authorized clients."
    />
    <ClientTable
      v-if="rows.length"
      :clients="rows"
      @revoke="revoke"
      @change-accounts="changeAccounts"
    />

    <h2>Authorize an existing certificate</h2>
    <AuthorizeForm ref="form" @submit="submit" />
  </section>
</template>
