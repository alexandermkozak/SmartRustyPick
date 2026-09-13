<script lang="ts" setup>
/** What a restore did, or - after a verify - what it would do. */
import {bytes, count} from '@shared/format'
import type {ImportReport} from '../types'

const props = defineProps<{report: ImportReport}>()
</script>

<template>
  <section class="card">
    <h3>{{ props.report.dryRun ? 'Verified' : 'Restored' }}</h3>
    <p class="note">
      {{ props.report.source }}, taken {{ props.report.takenUtc }}.
      {{ count(props.report.records) }} record(s).
      <template v-if="props.report.dryRun"> Nothing was written. </template>
      <template v-else-if="props.report.accountsCreated.length">
        Account(s) created: {{ props.report.accountsCreated.join(', ') }}.
      </template>
    </p>

    <table>
      <thead>
        <tr>
          <th>Account</th>
          <th>File</th>
          <th>{{ props.report.dryRun ? 'Would be' : 'Action' }}</th>
          <th class="num">Records</th>
          <th class="num">Dictionary</th>
          <th class="num">Size</th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="file in props.report.files" :key="`${file.account}/${file.file}`">
          <td>{{ file.account }}</td>
          <td>{{ file.file }}</td>
          <td>{{ file.action }}</td>
          <td class="num">{{ count(file.records) }}</td>
          <td class="num">{{ count(file.dictionary) }}</td>
          <td class="num">{{ bytes(file.bytes) }}</td>
        </tr>
      </tbody>
    </table>
  </section>
</template>
