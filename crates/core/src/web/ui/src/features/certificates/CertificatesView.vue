<script lang="ts" setup>
import {ref} from 'vue'
import IssueForm from './components/IssueForm.vue'
import IssuedCertificate from './components/IssuedCertificate.vue'
import {useCertificateIssuing} from './composables/useCertificateIssuing'
import type {CertificateRequest} from './types'

const {issuing, issued, files, issue} = useCertificateIssuing()
const form = ref<InstanceType<typeof IssueForm> | null>(null)

async function submit(request: CertificateRequest): Promise<void> {
  if (await issue(request)) form.value?.reset()
}
</script>

<template>
  <section>
    <h2>Issue a client certificate</h2>
    <p class="note">
      The certificate is signed by the server's CA and authorized in the same step, so it can
      connect as soon as it is downloaded. The private key is generated on the server and shown
      once, here.
    </p>

    <IssueForm ref="form" :issuing="issuing" @submit="submit" />
    <IssuedCertificate v-if="issued" :certificate="issued" :files="files" />
  </section>
</template>
