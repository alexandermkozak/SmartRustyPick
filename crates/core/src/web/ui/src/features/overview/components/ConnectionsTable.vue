<script lang="ts" setup>
/** The sessions holding a TLS connection right now. */
import RolePill from '@shared/components/RolePill.vue'
import {count, duration} from '@shared/format'
import type {ConnectionSnapshot} from '../types'

defineProps<{connections: ConnectionSnapshot[]}>()
</script>

<template>
  <div class="table-wrap">
    <table>
      <thead>
        <tr>
          <th>Client</th>
          <th>Peer</th>
          <th>Role</th>
          <th class="num">Connected</th>
          <th class="num">Requests</th>
          <th>Last command</th>
          <th class="num">Idle</th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="connection in connections" :key="connection.id">
          <td>{{ connection.client_name || '—' }}</td>
          <td class="mono">{{ connection.peer }}</td>
          <td>
            <RolePill :is-admin="connection.is_admin" />
          </td>
          <td class="num">{{ duration(connection.connected_seconds) }}</td>
          <td class="num">{{ count(connection.requests) }}</td>
          <td>{{ connection.last_command || '—' }}</td>
          <td class="num">{{ duration(connection.idle_seconds) }}</td>
        </tr>
        <tr v-if="!connections.length">
          <td class="empty" colspan="7">No open connections.</td>
        </tr>
      </tbody>
    </table>
  </div>
</template>
