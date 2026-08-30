<script lang="ts" setup>
import {computed} from 'vue'
import AccountList from './components/AccountList.vue'
import FileList from './components/FileList.vue'
import FileStatistics from './components/FileStatistics.vue'
import DictionaryTable from './components/DictionaryTable.vue'
import DictionaryForm from './components/DictionaryForm.vue'
import {useAccountBrowser} from './composables/useAccountBrowser'
import {useFileDictionary} from './composables/useFileDictionary'

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

const rows = computed(() => accounts.data.value ?? [])
const entryNames = computed(() => dictionary.entries.value.map((entry) => entry.name))
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
</template>
