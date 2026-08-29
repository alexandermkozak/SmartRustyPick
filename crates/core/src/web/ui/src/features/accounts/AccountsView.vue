<script lang="ts" setup>
import {computed} from 'vue'
import AccountList from './components/AccountList.vue'
import FileList from './components/FileList.vue'
import FileStatistics from './components/FileStatistics.vue'
import {useAccountBrowser} from './composables/useAccountBrowser'

const {
  accounts,
  selectedAccount,
  selectedFile,
  files,
  filesLoaded,
  stats,
  selectAccount,
  selectFile,
} = useAccountBrowser()

const rows = computed(() => accounts.data.value ?? [])
</script>

<template>
  <section class="split">
    <div>
      <AccountList
        :accounts="rows"
        :error="accounts.error.value"
        :loaded="accounts.loaded.value"
        :selected="selectedAccount"
        @select="selectAccount"
      />
    </div>
    <div>
      <FileList
        :account="selectedAccount"
        :files="files"
        :loaded="filesLoaded"
        :selected="selectedFile"
        @select="selectFile"
      />
    </div>
    <div>
      <FileStatistics :stats="stats" />
    </div>
  </section>
</template>
