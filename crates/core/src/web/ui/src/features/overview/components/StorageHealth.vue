<script lang="ts" setup>
/**
 * Storage health across every account, on the tab a person already has open.
 *
 * The gap this closes is not that the information was missing - the Accounts
 * tab has had it all along - but that nothing reported it unless somebody went
 * looking. A database with forty files has no view that says "these are the
 * three worth your attention", and so nobody ever finds out. This is that view,
 * and the point of it is that nobody has to remember to check.
 *
 * The verdicts are the cheap ones: section metadata and index `state` files, no
 * group trailers and no records. Enough to say which account is worth opening.
 */
import {computed} from 'vue'
import HealthPill from '@shared/components/HealthPill.vue'
import {count} from '@shared/format'
import {rollUp, verdictOf} from '@shared/health'
import type {AccountHealth} from '../types'

const props = defineProps<{accounts: AccountHealth[]; error: string | null}>()

const totals = computed(() => {
  const accounts = props.accounts
  return {
    files: accounts.reduce((sum, account) => sum + account.file_count, 0),
    indexes: accounts.reduce((sum, account) => sum + (account.index_count ?? 0), 0),
    stale: accounts.reduce((sum, account) => sum + (account.stale_indexes ?? 0), 0),
    unhealthy: accounts.reduce((sum, account) => sum + (account.unhealthy_files ?? 0), 0),
    verdict: rollUp(accounts.map((account) => verdictOf(account.health?.verdict))),
  }
})

/** The accounts with something to say, worst first by how much of it there is. */
const worrying = computed(() =>
  props.accounts
    .filter((account) => verdictOf(account.health?.verdict) !== 'good')
    .sort((a, b) => (b.unhealthy_files ?? 0) - (a.unhealthy_files ?? 0)),
)
</script>

<template>
  <h2>Storage</h2>
  <div class="card">
    <p v-if="error" class="alert inline">{{ error }}</p>
    <p v-else-if="!accounts.length" class="empty">No accounts.</p>
    <template v-else>
      <p class="health-summary">
        <HealthPill :verdict="totals.verdict" show-good />
        <span>
          {{ count(accounts.length) }} accounts · {{ count(totals.files) }} files ·
          {{ count(totals.indexes) }} indexes
        </span>
        <span v-if="totals.stale">{{ count(totals.stale) }} stale</span>
      </p>
      <p v-if="!worrying.length" class="note">
        Every file's format, checksums and indexes are current. Open a file on the Accounts tab for
        its group distribution and the rest of the measures.
      </p>
      <ul v-else class="list">
        <li v-for="account in worrying" :key="account.name" class="entry">
          <span class="row">
            <span class="mono">{{ account.name }}</span>
            <HealthPill :verdict="verdictOf(account.health?.verdict)" />
          </span>
          <span class="meta">
            {{ (account.health?.reasons ?? []).join(' · ') || 'needs attention' }}
          </span>
        </li>
      </ul>
    </template>
  </div>
</template>
