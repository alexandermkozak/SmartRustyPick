<script lang="ts" setup>
/** Chooses an archive and says what may be done with it. */
import {computed, reactive, ref} from 'vue'
import type {ImportOptions} from '../types'

const props = defineProps<{importing: boolean}>()
const emit = defineEmits<{submit: [archive: File, options: ImportOptions]}>()

const form = reactive<ImportOptions>({into: '', overwrite: false, verify: true})
const chosen = ref<File | null>(null)
const input = ref<HTMLInputElement | null>(null)

const ready = computed(() => chosen.value !== null)

function choose(event: Event): void {
  chosen.value = (event.target as HTMLInputElement).files?.[0] ?? null
}

function submit(): void {
  if (chosen.value) emit('submit', chosen.value, {...form})
}

function reset(): void {
  chosen.value = null
  if (input.value) input.value.value = ''
}

defineExpose({reset})
</script>

<template>
  <form class="card form" @submit.prevent="submit">
    <label>
      Archive
      <input ref="input" accept=".srp" required type="file" @change="choose" />
    </label>
    <label>
      Restore into
      <input v-model="form.into" autocomplete="off" placeholder="The account the archive names" />
    </label>

    <label class="check">
      <input v-model="form.verify" type="checkbox" />
      Verify only - report what would happen and write nothing
    </label>
    <label class="check">
      <input v-model="form.overwrite" type="checkbox" />
      Replace files that already exist
    </label>

    <p v-if="form.overwrite && !form.verify" class="hint error-text">
      A restore replaces rather than merges. Records these files have gained since the archive was
      taken will be gone.
    </p>
    <p v-else-if="!form.overwrite" class="hint">
      Without this, an archive landing on a file that already exists is refused whole and nothing is
      written.
    </p>

    <button :disabled="props.importing || !ready" type="submit">
      {{ props.importing ? 'Reading the archive…' : form.verify ? 'Verify' : 'Restore' }}
    </button>
  </form>
</template>
