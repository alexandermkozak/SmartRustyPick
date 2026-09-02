<script lang="ts" setup>
import {computed} from 'vue'
import AccountList from './components/AccountList.vue'
import FileList from './components/FileList.vue'
import FileStatistics from './components/FileStatistics.vue'
import DictionaryTable from './components/DictionaryTable.vue'
import DictionaryForm from './components/DictionaryForm.vue'
import IndexTable from './components/IndexTable.vue'
import IndexForm from './components/IndexForm.vue'
import {useAccountBrowser} from './composables/useAccountBrowser'
import {useFileDictionary} from './composables/useFileDictionary'
import {useFileIndexes} from './composables/useFileIndexes'

const {
  accounts,
  selectedAccount,
  selectedFile,
  files,
  filesLoaded,
  stats,
  changing,
  maintaining,
  selectAccount,
  selectFile,
  setDurable,
  createAccount,
  deleteAccount,
  createFile,
  deleteFile,
} = useAccountBrowser()

const dictionary = useFileDictionary(selectedAccount, selectedFile)
// The index list is derived from the same dictionary the section above edits,
// so the field a new index is offered on is one this page has already shown.
const indexes = useFileIndexes(selectedAccount, selectedFile, dictionary.entries)

const rows = computed(() => accounts.data.value ?? [])
const entryNames = computed(() => dictionary.entries.value.map((entry) => entry.name))
const recordCount = computed(() => stats.value?.record_count ?? 0)

/**
 * Re-reads the file's statistics after an index changes, so the panel's index
 * count and disk figure describe what is now there. The list itself is re-read
 * by the composable; this is the part of the page that sits outside it.
 */
async function afterIndexChange(done: boolean): Promise<void> {
  if (done && selectedFile.value) await selectFile(selectedFile.value)
}
</script>

<template>
  <section class="split">
    <div>
      <AccountList
        :accounts="rows"
        :busy="maintaining"
        :error="accounts.error.value"
        :loaded="accounts.loaded.value"
        :selected="selectedAccount"
        @select="selectAccount"
        @create="createAccount"
        @drop="deleteAccount"
      />
    </div>
    <div>
      <FileList
        :account="selectedAccount"
        :busy="maintaining"
        :files="files"
        :loaded="filesLoaded"
        :selected="selectedFile"
        @select="selectFile"
        @create="createFile"
        @drop="deleteFile"
      />
    </div>
    <div>
      <FileStatistics :changing="changing" :stats="stats" @set-durable="setDurable" />
    </div>
  </section>

  <!-- The dictionary is the file's shape rather than its contents, so it is the
       one part of a file this page edits. It gets the full width: six columns
       and a form do not fit in a third of it. -->
  <section v-if="selectedAccount && selectedFile" class="dictionary">
    <h2>Dictionary of {{ selectedAccount }}/{{ selectedFile }}</h2>
    <DictionaryTable
      :busy="dictionary.saving.value"
      :editing="dictionary.editing.value"
      :entries="dictionary.entries.value"
      :loaded="dictionary.loaded.value"
      @edit="dictionary.edit"
      @drop="dictionary.remove"
    />
    <DictionaryForm
      :draft="dictionary.draft.value"
      :editing="dictionary.editing.value"
      :entries="entryNames"
      :saving="dictionary.saving.value"
      @submit="dictionary.save"
      @cancel="dictionary.reset"
    />
  </section>

  <!-- Indexes come after the dictionary because they are built on it: an index
       is on a field the dictionary defines, and the numbers here say whether
       that field was worth indexing. -->
  <section v-if="selectedAccount && selectedFile" class="indexes">
    <h2>Indexes on {{ selectedAccount }}/{{ selectedFile }}</h2>
    <IndexTable
      :busy="indexes.working.value"
      :indexes="indexes.indexes.value"
      :loaded="indexes.loaded.value"
      :records="recordCount"
      @rebuild="(field) => indexes.rebuild(field).then(afterIndexChange)"
      @drop="(field) => indexes.remove(field).then(afterIndexChange)"
    />
    <IndexForm
      v-model="indexes.field.value"
      :candidates="indexes.candidates.value"
      :has-dictionary="entryNames.length > 0"
      :saving="indexes.working.value"
      @submit="indexes.create().then(afterIndexChange)"
    />
  </section>
</template>
