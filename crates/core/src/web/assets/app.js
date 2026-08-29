/* Dashboard behaviour.
 *
 * Every call goes to this page's own origin and is answered by one remote
 * protocol command. Values coming back from the database are written with
 * textContent, never as markup: record keys, account names and client names are
 * user data, and the page must not become a way to run something with them.
 */

const REFRESH_MS = 5000;

const state = {
    view: 'overview',
    account: null,
    file: null,
    timer: null,
};

/* ---------- helpers ---------- */

function el(tag, props = {}, children = []) {
    const node = document.createElement(tag);
    for (const [key, value] of Object.entries(props)) {
        if (key === 'class') {
            node.className = value;
        } else if (key === 'text') {
            node.textContent = value;
        } else if (key.startsWith('on')) {
            node.addEventListener(key.slice(2), value);
        } else if (value !== null && value !== undefined) {
            node.setAttribute(key, value);
        }
    }
    for (const child of [].concat(children)) {
        node.appendChild(typeof child === 'string' ? document.createTextNode(child) : child);
    }
    return node;
}

function replace(node, children) {
    node.replaceChildren(...[].concat(children));
}

async function api(path, options = {}) {
    const response = await fetch(path, {
        credentials: 'same-origin',
        headers: options.body ? {'Content-Type': 'application/json'} : {},
        ...options,
    });
    let payload = {};
    try {
        payload = await response.json();
    } catch (_) {
        // A body that is not JSON is a bug on our side; the status still says enough.
    }
    if (!response.ok) {
        throw new Error(payload.error || `Request failed (${response.status})`);
    }
    return payload;
}

function showError(message) {
    const alert = document.getElementById('alert');
    alert.textContent = message;
    alert.hidden = false;
}

function clearError() {
    document.getElementById('alert').hidden = true;
}

function duration(seconds) {
    if (seconds === null || seconds === undefined) return '—';
    const units = [['d', 86400], ['h', 3600], ['m', 60]];
    let remaining = Math.max(0, Math.floor(seconds));
    const parts = [];
    for (const [suffix, size] of units) {
        if (remaining >= size) {
            parts.push(`${Math.floor(remaining / size)}${suffix}`);
            remaining %= size;
        }
        if (parts.length === 2) break;
    }
    if (parts.length < 2) parts.push(`${remaining}s`);
    return parts.join(' ');
}

function bytes(value) {
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    let size = Number(value) || 0;
    let unit = 0;
    while (size >= 1024 && unit < units.length - 1) {
        size /= 1024;
        unit += 1;
    }
    return `${unit === 0 ? size : size.toFixed(1)} ${units[unit]}`;
}

function number(value) {
    return Number(value || 0).toLocaleString();
}

/* ---------- overview ---------- */

function statCard(label, value) {
    return el('div', {class: 'stat'}, [
        el('div', {class: 'value', text: value}),
        el('div', {class: 'label', text: label}),
    ]);
}

async function loadOverview() {
    const stats = (await api('/api/stats')).record || {};
    document.getElementById('server-line').textContent =
        `${stats.listen_addr || 'unknown'} · up ${duration(stats.uptime_seconds)}`;
    const health = document.getElementById('health');
    health.textContent = 'connected';
    health.className = 'pill up';

    replace(document.getElementById('stat-grid'), [
        statCard('Active connections', number((stats.active_connections || []).length)),
        statCard('Connections served', number(stats.total_connections)),
        statCard('Rejected', number(stats.rejected_connections)),
        statCard('Requests', number(stats.total_requests)),
        statCard('Failed requests', number(stats.failed_requests)),
        statCard('Pending writes', number(stats.pending_writes)),
        statCard('Tables in memory', number(stats.loaded_tables)),
        statCard('Authorized clients', number(stats.authorized_clients)),
    ]);

    const rows = (stats.active_connections || []).map((connection) => el('tr', {}, [
        el('td', {text: connection.client_name || '—'}),
        el('td', {class: 'mono', text: connection.peer}),
        el('td', {}, [el('span', {
            class: connection.is_admin ? 'tag admin' : 'tag',
            text: connection.is_admin ? 'admin' : 'client',
        })]),
        el('td', {class: 'num', text: duration(connection.connected_seconds)}),
        el('td', {class: 'num', text: number(connection.requests)}),
        el('td', {text: connection.last_command || '—'}),
        el('td', {class: 'num', text: duration(connection.idle_seconds)}),
    ]));
    replace(
        document.querySelector('#connections tbody'),
        rows.length ? rows : [el('tr', {}, [el('td', {colspan: '7', class: 'empty', text: 'No open connections.'})])],
    );
}

/* ---------- authorizations ---------- */

async function loadClients() {
    const clients = (await api('/api/clients')).results || [];
    const rows = clients.map(([name, info]) => el('tr', {}, [
        el('td', {text: name}),
        el('td', {class: 'mono', title: info.thumbprint, text: `${String(info.thumbprint || '').slice(0, 16)}…`}),
        el('td', {text: (info.accounts || []).join(', ') || (info.is_admin ? 'all' : '—')}),
        el('td', {}, [el('span', {
            class: info.is_admin ? 'tag admin' : 'tag',
            text: info.is_admin ? 'admin' : 'client',
        })]),
        el('td', {}, [
            el('button', {
                class: 'small',
                type: 'button',
                onclick: () => changeAccounts(name, false),
            }, 'Add account'),
            ' ',
            el('button', {
                class: 'small',
                type: 'button',
                onclick: () => changeAccounts(name, true),
            }, 'Remove account'),
            ' ',
            el('button', {
                class: 'small danger',
                type: 'button',
                onclick: () => revoke(name),
            }, 'Revoke'),
        ]),
    ]));
    replace(
        document.querySelector('#clients tbody'),
        rows.length ? rows : [el('tr', {}, [el('td', {colspan: '5', class: 'empty', text: 'No authorized clients.'})])],
    );
}

async function revoke(name) {
    if (!window.confirm(`Revoke "${name}"? Its certificate stops working immediately.`)) return;
    await guard(async () => {
        await api(`/api/clients/${encodeURIComponent(name)}`, {method: 'DELETE'});
        await loadClients();
    });
}

async function changeAccounts(name, remove) {
    const answer = window.prompt(`${remove ? 'Remove' : 'Add'} accounts for "${name}" (comma separated):`, '');
    if (!answer) return;
    await guard(async () => {
        await api(`/api/clients/${encodeURIComponent(name)}/accounts`, {
            method: 'POST',
            body: JSON.stringify({accounts: answer, remove}),
        });
        await loadClients();
    });
}

/* ---------- certificates ---------- */

function download(filename, contents) {
    const url = URL.createObjectURL(new Blob([contents], {type: 'application/x-pem-file'}));
    const link = el('a', {href: url, download: filename});
    document.body.appendChild(link);
    link.click();
    link.remove();
    setTimeout(() => URL.revokeObjectURL(url), 10000);
}

function certificateResult(cert) {
    const parts = [
        ['Certificate', `${cert.common_name}.crt`, cert.certificate_pem],
        ['Private key', `${cert.common_name}.key`, cert.private_key_pem],
        ['CA certificate', 'ca.crt', cert.ca_pem],
    ];
    return el('div', {class: 'card'}, [
        el('p', {class: 'success', text: `Issued and authorized as "${cert.common_name}".`}),
        el('dl', {class: 'stats'}, [
            el('dt', {text: 'Thumbprint'}),
            el('dd', {class: 'mono', text: cert.thumbprint}),
            el('dt', {text: 'Server path'}),
            el('dd', {class: 'mono', text: cert.cert_path}),
            el('dt', {text: 'PKCS#12'}),
            el('dd', {class: 'mono', text: cert.pfx_path || 'not generated'}),
        ]),
        el('div', {class: 'downloads'}, parts.map(([label, filename, contents]) => el('button', {
            type: 'button',
            class: 'small',
            onclick: () => download(filename, contents),
        }, `Download ${label.toLowerCase()}`))),
        el('p', {class: 'empty', text: 'The private key is shown once. Copy it now if the download is blocked.'}),
        el('textarea', {readonly: 'readonly', spellcheck: 'false'}, parts.map(([, , contents]) => contents).join('\n')),
    ]);
}

/* ---------- accounts ---------- */

async function loadAccounts() {
    const accounts = (await api('/api/accounts')).results || [];
    const items = accounts.map(([name, info]) => el('li', {}, [
        el('button', {
            type: 'button',
            'aria-current': String(state.account === name),
            onclick: () => selectAccount(name),
        }, [
            name,
            el('span', {
                class: 'meta',
                text: `${number(info.file_count)} files · ${number(info.record_count)} records · ${bytes(info.disk_bytes)}`,
            }),
            el('span', {class: 'meta', text: info.directory}),
        ]),
    ]));
    replace(
        document.getElementById('account-list'),
        items.length ? items : [el('li', {class: 'empty', text: 'No accounts.'})],
    );
}

async function selectAccount(name) {
    state.account = name;
    state.file = null;
    document.getElementById('files-heading').textContent = `Files in ${name}`;
    replace(document.getElementById('file-stats'), el('p', {class: 'empty', text: 'Select a file.'}));
    await guard(async () => {
        await loadAccounts();
        const files = (await api(`/api/accounts/${encodeURIComponent(name)}/files`)).keys || [];
        const items = files.map((file) => el('li', {}, [
            el('button', {
                type: 'button',
                'aria-current': String(state.file === file),
                onclick: () => selectFile(file),
            }, file),
        ]));
        replace(
            document.getElementById('file-list'),
            items.length ? items : [el('li', {class: 'empty', text: 'No files in this account.'})],
        );
    });
}

async function selectFile(file) {
    state.file = file;
    await guard(async () => {
        const stats = (await api(
            `/api/accounts/${encodeURIComponent(state.account)}/files/${encodeURIComponent(file)}`,
        )).record || {};
        const rows = [
            ['Records', number(stats.record_count)],
            ['Dictionary entries', number(stats.dict_count)],
            ['Hash modulus', number(stats.modulus)],
            ['Group files', number(stats.group_count)],
            ['Smallest group', bytes(stats.smallest_group_bytes)],
            ['Largest group', bytes(stats.largest_group_bytes)],
            ['On disk', bytes(stats.disk_bytes)],
            ['Flush version', number(stats.version)],
            ['Durable writes', stats.durable ? 'yes' : 'no'],
            ['Checksums', stats.checksums ? 'yes' : 'no'],
            ['Format', stats.legacy ? 'legacy flat file' : 'hashed'],
            ['In memory', stats.loaded ? 'yes' : 'no'],
            ['Last modified', stats.modified_seconds_ago === null || stats.modified_seconds_ago === undefined
                ? '—'
                : `${duration(stats.modified_seconds_ago)} ago`],
        ];
        replace(document.getElementById('file-stats'), [
            el('h3', {class: 'mono', text: `${stats.account}/${stats.name}`}),
            el('dl', {class: 'stats'}, rows.flatMap(([label, value]) => [
                el('dt', {text: label}),
                el('dd', {text: value}),
            ])),
        ]);
        // Re-render the file list so the selected entry is marked.
        for (const button of document.querySelectorAll('#file-list button')) {
            button.setAttribute('aria-current', String(button.textContent === file));
        }
    });
}

/* ---------- wiring ---------- */

async function guard(work) {
    try {
        await work();
        clearError();
    } catch (error) {
        showError(error.message);
        const health = document.getElementById('health');
        health.textContent = 'error';
        health.className = 'pill down';
    }
}

async function refresh() {
    if (state.view === 'overview') {
        await guard(loadOverview);
    } else if (state.view === 'clients') {
        await guard(loadClients);
    } else if (state.view === 'accounts') {
        await guard(loadAccounts);
    }
}

function showView(view) {
    state.view = view;
    for (const tab of document.querySelectorAll('.tab')) {
        tab.setAttribute('aria-current', String(tab.dataset.view === view));
    }
    for (const section of document.querySelectorAll('.view')) {
        section.hidden = section.id !== `view-${view}`;
    }
    refresh();
}

document.getElementById('tabs').addEventListener('click', (event) => {
    const tab = event.target.closest('.tab');
    if (tab) showView(tab.dataset.view);
});

document.getElementById('refresh').addEventListener('click', refresh);

document.getElementById('authorize-form').addEventListener('submit', async (event) => {
    event.preventDefault();
    const form = event.target;
    const data = new FormData(form);
    await guard(async () => {
        await api('/api/clients', {
            method: 'POST',
            body: JSON.stringify({
                name: data.get('name'),
                thumbprint: String(data.get('thumbprint') || '').trim().toLowerCase(),
                accounts: data.get('accounts') || '',
                is_admin: data.get('is_admin') === 'on',
            }),
        });
        form.reset();
        await loadClients();
    });
});

document.getElementById('cert-form').addEventListener('submit', async (event) => {
    event.preventDefault();
    const form = event.target;
    const data = new FormData(form);
    const button = form.querySelector('button');
    button.disabled = true;
    await guard(async () => {
        const response = await api('/api/certificates', {
            method: 'POST',
            body: JSON.stringify({
                common_name: data.get('common_name'),
                accounts: data.get('accounts') || '',
                is_admin: data.get('is_admin') === 'on',
            }),
        });
        replace(document.getElementById('cert-result'), certificateResult(response.record || {}));
        form.reset();
    });
    button.disabled = false;
});

state.timer = setInterval(() => {
    if (state.view === 'overview') refresh();
}, REFRESH_MS);

showView('overview');
