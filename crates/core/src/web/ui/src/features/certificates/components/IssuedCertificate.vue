<script lang="ts" setup>
/** What came back: where it was written, and the material to take away. */
import {computed} from 'vue'
import StatList from '@shared/components/StatList.vue'
import {download, type DownloadableFile} from '../composables/useCertificateIssuing'
import type {GeneratedCert} from '../types'

const props = defineProps<{certificate: GeneratedCert; files: DownloadableFile[]}>()

const details = computed<Array<[string, string]>>(() => [
  ['Thumbprint', props.certificate.thumbprint],
  ['Certificate', props.certificate.cert_path],
  ['Private key', props.certificate.key_path],
  ['PKCS#12', props.certificate.pfx_path ?? 'not generated'],
])

const allPem = computed(() => props.files.map((file) => file.contents).join('\n'))
</script>

<template>
  <div class="card issued">
    <p class="success">Issued and authorized as "{{ certificate.common_name }}".</p>
    <StatList :rows="details" mono />
    <div class="downloads">
      <button
        v-for="file in files"
        :key="file.filename"
        class="small"
        type="button"
        @click="download(file.filename, file.contents)"
      >
        Download {{ file.label }}
      </button>
    </div>
    <p class="empty">The private key is shown once. Copy it now if the download is blocked.</p>
    <textarea :value="allPem" readonly spellcheck="false"></textarea>
  </div>
</template>
