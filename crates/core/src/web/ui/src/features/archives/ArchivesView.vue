<script lang="ts" setup>
import {onMounted, ref} from 'vue'
import ExportPanel from './components/ExportPanel.vue'
import ImportPanel from './components/ImportPanel.vue'
import RestoreReport from './components/RestoreReport.vue'
import {useArchives} from './composables/useArchives'
import type {ExportScope, ImportOptions} from './types'

const {accounts, exporting, importing, report, loadAccounts, exportArchive, importArchive} =
  useArchives()
const panel = ref<InstanceType<typeof ImportPanel> | null>(null)

onMounted(loadAccounts)

async function restore(archive: File, options: ImportOptions): Promise<void> {
  // The chosen file is cleared only after a restore that actually ran, so a
  // verify leaves it in place for the restore that usually follows it.
  if ((await importArchive(archive, options)) && !options.verify) {
    panel.value?.reset()
    await loadAccounts()
  }
}

function save(scope: ExportScope): void {
  void exportArchive(scope)
}
</script>

<template>
  <section>
    <h2>Back up</h2>
    <p class="note">
      An archive is the records, the dictionaries, each file's type and flags and its index
      definitions - taken while the server runs, with a checksum over the whole of it. It carries
      records rather than the storage layout, so it restores into this server or a different one
      alike. Copying
      <code>db_storage/</code> is not a backup: writes are buffered, and a copy can catch a flush
      half done.
    </p>
    <ExportPanel :accounts="accounts" :exporting="exporting" @submit="save" />

    <h2>Restore</h2>
    <p class="note">
      Nothing is written until the archive has decoded whole and every file in it has been checked
      against what is already here, so a truncated or altered archive restores nothing at all.
      Verify first - it reports exactly what a restore would do and writes nothing.
    </p>
    <ImportPanel ref="panel" :importing="importing" @submit="restore" />
    <RestoreReport v-if="report" :report="report" />
  </section>
</template>
