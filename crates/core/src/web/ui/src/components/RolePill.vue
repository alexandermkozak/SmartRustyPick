<script lang="ts" setup>
defineProps<{ isAdmin: boolean }>()
</script>

<template>
  <span :class="{ admin: isAdmin }" class="tag">{{ isAdmin ? 'admin' : 'client' }}</span>
</template>
