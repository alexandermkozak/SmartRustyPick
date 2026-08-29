<script lang="ts" setup>
import {computed, reactive, ref} from 'vue'
import StatList from '../components/StatList.vue'
import {api} from '../api'
import {useAlerts} from '../composables/useAlerts'
import type {GeneratedCert} from '../types'

const alerts = useAlerts()
const form = reactive({common_name: '', accounts: '', is_admin: false})
const issuing = ref(false)
const issued = ref<GeneratedCert | null>(null)

const details = computed<Array<[string, string]>>(() => {
  const cert = issued.value
  if (!cert) return []
  return [
    ['Thumbprint', cert.thumbprint],
    ['Certificate', cert.cert_path],
    ['Private key', cert.key_path],
    ['PKCS#12', cert.pfx_path ?? 'not generated'],
  ]
})

/** The three files a client needs, in the order it needs them. */
const files = computed(() => {
  const cert = issued.value
  if (!cert) return []
  return [
    {label: 'certificate', filename: `${cert.common_name}.crt`, contents: cert.certificate_pem},
    {label: 'private key', filename: `${cert.common_name}.key`, contents: cert.private_key_pem},
    {label: 'CA certificate', filename: 'ca.crt', contents: cert.ca_pem},
  ]
})

const allPem = computed(() => files.value.map((file) => file.contents).join('\n'))

async function generate(): Promise<void> {
  issuing.value = true
  try {
    issued.value = await api.generateCertificate({
      common_name: form.common_name.trim(),
      accounts: form.accounts
          .split(',')
          .map((account) => account.trim())
          .filter(Boolean),
      is_admin: form.is_admin,
    })
    alerts.clear()
    Object.assign(form, {common_name: '', accounts: '', is_admin: false})
  } catch (cause) {
    alerts.fail(cause)
  } finally {
    issuing.value = false
  }
}

function download(filename: string, contents: string): void {
  const url = URL.createObjectURL(new Blob([contents], {type: 'application/x-pem-file'}))
  const link = document.createElement('a')
  link.href = url
  link.download = filename
  document.body.appendChild(link)
  link.click()
  link.remove()
  window.setTimeout(() => URL.revokeObjectURL(url), 10_000)
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

    <form class="card form" @submit.prevent="generate">
      <label>
        Common name
        <input
            v-model="form.common_name"
            autocomplete="off"
            pattern="[A-Za-z0-9._-]+"
            placeholder="reporting-service"
            required
            title="Letters, digits, dot, dash and underscore only"
        />
      </label>
      <label>
        Allowed accounts
        <input v-model="form.accounts" autocomplete="off" placeholder="SALES, REPORTS"/>
      </label>
      <label class="check">
        <input v-model="form.is_admin" type="checkbox"/>
        Administrator certificate
      </label>
      <p v-if="!form.is_admin && !form.accounts.trim()" class="hint">
        A non-admin certificate needs at least one allowed account.
      </p>
      <button :disabled="issuing" type="submit">
        {{ issuing ? 'Generating…' : 'Generate and authorize' }}
      </button>
    </form>

    <div v-if="issued" class="card issued">
      <p class="success">Issued and authorized as "{{ issued.common_name }}".</p>
      <StatList :rows="details" mono/>
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
  </section>
</template>
