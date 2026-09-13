<script lang="ts" setup>
/** Picks what to back up, and downloads it. */
import {computed, reactive, watch} from 'vue'
import type {ExportScope} from '../types'

const props = defineProps<{accounts: string[]; exporting: boolean}>()
const emit = defineEmits<{submit: [scope: ExportScope]}>()

const form = reactive<ExportScope>({account: '', file: ''})

// A file belongs to an account, so naming one without an account is not a
// scope the server can act on. Clearing it here means the form cannot be put
// into that state at all, rather than being refused once it is submitted.
watch(
  () => form.account,
  (account) => {
    if (!account) form.file = ''
  },
)

const everything = computed(() => form.account === '')
</script>

<template>
  <form class="card form" @submit.prevent="emit('submit', {...form})">
    <label>
      Account
      <select v-model="form.account">
        <option value="">Every account</option>
        <option v-for="account in props.accounts" :key="account" :value="account">
          {{ account }}
        </option>
      </select>
    </label>
    <label>
      File
      <input
        v-model="form.file"
        :disabled="everything"
        autocomplete="off"
        placeholder="All files in the account"
      />
    </label>

    <p v-if="everything" class="hint">
      A whole-database export holds every account still while it runs, so writes wait for it. SYSTEM
      is left out: it holds the certificate thumbprints this deployment authorized, which have no
      business travelling inside a backup.
    </p>
    <p v-else-if="!form.file" class="hint">
      The account's files are captured together, so a record written to one and a record written to
      another in the same act are either both in the archive or neither is.
    </p>

    <button :disabled="props.exporting" type="submit">
      {{ props.exporting ? 'Building the archive…' : 'Download archive' }}
    </button>
  </form>
</template>
