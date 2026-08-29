<script lang="ts" setup>
/**
 * The shell: a header, a tab per feature, and the banner errors surface in.
 *
 * It composes slices and owns none of their behaviour. The only things it
 * imports from a feature are the tab descriptors in the registry and the two
 * header widgets the overview slice publishes - so a new feature is a new
 * directory plus one line in `features/index.ts`.
 */
import {computed, shallowRef} from 'vue'
import {featureTabs} from './features'
import {ServerControls, ServerLine} from './features/overview'
import {useAlerts} from '@shared/composables/useAlerts'

const current = shallowRef(featureTabs[0].id)
const alerts = useAlerts()

const view = computed(
  () => (featureTabs.find((tab) => tab.id === current.value) ?? featureTabs[0]).component,
)

function select(id: string): void {
  current.value = id
  alerts.clear()
}
</script>

<template>
  <header class="bar">
    <div class="brand">
      <span class="mark">SRP</span>
      <div>
        <h1>SmartRustyPick</h1>
        <ServerLine />
      </div>
    </div>
    <div class="bar-right">
      <ServerControls />
    </div>
  </header>

  <nav class="tabs">
    <button
      v-for="tab in featureTabs"
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
      <component :is="view" />
    </KeepAlive>
  </main>
</template>
