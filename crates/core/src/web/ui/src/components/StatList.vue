<script lang="ts" setup>
/** A label/value definition list, used for file and certificate details. */
defineProps<{ rows: Array<[string, string]>; mono?: boolean }>()
</script>

<template>
  <dl class="stats">
    <template v-for="[label, value] in rows" :key="label">
      <dt>{{ label }}</dt>
      <dd :class="{ mono }">{{ value }}</dd>
    </template>
  </dl>
</template>
