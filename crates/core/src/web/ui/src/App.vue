<script lang="ts" setup>
import {computed, onMounted, onUnmounted, shallowRef} from 'vue'
import OverviewView from './views/OverviewView.vue'
import ClientsView from './views/ClientsView.vue'
import CertificatesView from './views/CertificatesView.vue'
import AccountsView from './views/AccountsView.vue'
import {useAlerts} from './composables/useAlerts'
import {useServerStats} from './composables/useServerStats'
import {duration} from './format'

const tabs = [
  {id: 'overview', label: 'Overview', component: OverviewView},
  {id: 'clients', label: 'Authorizations', component: ClientsView},
  {id: 'certificates', label: 'Certificates', component: CertificatesView},
  {id: 'accounts', label: 'Accounts', component: AccountsView},
] as const

type TabId = (typeof tabs)[number]['id']

const current = shallowRef<TabId>('overview')
const alerts = useAlerts()
const stats = useServerStats()

const view = computed(() => tabs.find((tab) => tab.id === current.value)!.component)

const serverLine = computed(() => {
  const snapshot = stats.data.value
  if (!snapshot) return stats.error.value ? 'not responding' : 'connecting…'
  return `${snapshot.listen_addr} · up ${duration(snapshot.uptime_seconds)}`
})

const health = computed(() => {
  if (stats.error.value) return {text: stats.live.value ? 'error' : 'disconnected', tone: 'down'}
  if (!stats.loaded.value) return {text: '…', tone: ''}
  return {text: 'connected', tone: 'up'}
})

function select(tab: TabId): void {
  current.value = tab
  alerts.clear()
}

// The shared poller outlives every view, so the shell owns its lifetime.
onMounted(() => stats.start())
onUnmounted(() => stats.stop())
</script>

<template>
  <header class="bar">
    <div class="brand">
      <span class="mark">SRP</span>
      <div>
        <h1>SmartRustyPick</h1>
        <p class="sub">{{ serverLine }}</p>
      </div>
    </div>
    <div class="bar-right">
      <span :class="health.tone" class="pill">{{ health.text }}</span>
      <label class="check live-toggle">
        <input
            :checked="stats.live.value"
            type="checkbox"
            @change="stats.live.value ? stats.stop() : stats.start()"
        />
        Live
      </label>
      <button :disabled="stats.loading.value" class="ghost" type="button" @click="stats.refresh()">
        Refresh
      </button>
    </div>
  </header>

  <nav class="tabs">
    <button
        v-for="tab in tabs"
        :key="tab.id"
        :aria-current="current === tab.id"
        class="tab"
        type="button"
        @click="select(tab.id)"
    >
      {{ tab.label }}
    </button>
  </nav>

  <p v-if="alerts.message.value" class="alert">{{ alerts.message.value }}</p>

  <main>
    <!-- Kept alive so switching tabs does not throw away a loaded file list or
         a certificate that has only just been shown. -->
    <KeepAlive>
      <component :is="view"/>
    </KeepAlive>
  </main>
</template>
