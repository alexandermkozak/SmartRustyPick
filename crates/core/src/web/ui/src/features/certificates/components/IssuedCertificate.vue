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

/**
 * The bundle's import passphrase. The server generates it per issuance and
 * keeps no copy, so this render is the only place it will ever exist - which is
 * why it gets its own block rather than a row in the list above.
 */
const passphrase = computed(() => props.certificate.pfx_passphrase)

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
    <div v-if="passphrase" class="passphrase">
      <p><strong>PKCS#12 passphrase</strong> — needed to import the bundle, and shown only here.</p>
      <code>{{ passphrase }}</code>
      <p class="empty">
        The server keeps no copy. Send it to whoever imports the bundle by some other route than the
        bundle itself; re-issue the certificate if it is lost.
      </p>
    </div>
    <p class="empty">The private key is shown once. Copy it now if the download is blocked.</p>
    <textarea :value="allPem" readonly spellcheck="false"></textarea>
  </div>
</template>
