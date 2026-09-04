# Web Management Dashboard

A browser interface for running the database: who may connect, what certificates they hold, what the server is doing
right now, and what the accounts and files look like. It starts with the database server, so there is nothing extra to
run.

It is a *management* interface, not a data browser. It reports how many records a file holds and how they are laid out;
it never returns one. A file's dictionary is the exception that proves the rule: it is the file's shape rather than its
contents, and maintaining it is why an operator opens a management interface at all.

## Starting it

The dashboard comes up with the protocol server, whichever way that server is started —
`make run-server`, the CLI's background service, or `START.SERVER`. On startup it prints the address to open:

```
Server listening on TLS 127.0.0.1:8443
Web dashboard on http://127.0.0.1:8080/?token=6f1c…
  authorized as WEB.DASHBOARD (thumbprint 5bfc…), reissued on every start
```

Open that URL. The token in it is stored in a `HttpOnly`, `SameSite=Strict` cookie, so the page's own requests carry it
without any script being able to read it.

### Configuration

| Setting       | Default     | Description                                                           |
|---------------|-------------|-----------------------------------------------------------------------|
| `web_enabled` | `true`      | Set `false` for a server that should expose nothing but the protocol. |
| `web_addr`    | `127.0.0.1` | Interface to bind. May carry its own port (`"0.0.0.0:9000"`).         |
| `web_port`    | `8080`      | Port to listen on.                                                    |
| `web_token`   | *generated* | A fixed access token. Unset, a new one is generated on every boot.    |

A failure to start the dashboard — a port already in use, a missing CA — is reported and does not stop the database from
serving.

## How it talks to the database

The dashboard holds a client certificate and speaks the ordinary
[remote protocol](protocol.md) over the same TLS listener as every other client. It has no private path into the engine,
so it can do exactly what its authorization allows and nothing more, and its work shows up in `LIST.CONNS` and
`SERVER.STATS` like anyone else's.

```
browser ──HTTP(localhost)──▶ dashboard ──TLS + client cert──▶ protocol server ──▶ engine
```

Its certificate is **issued fresh on every boot** and authorized under the fixed name
`WEB.DASHBOARD`, which replaces the previous entry. Two things follow:

- A dashboard certificate from an earlier run stops working the moment the server restarts. It is valid for a day at
  most in any case.
- `DEAUTHORIZE.CONN WEB.DASHBOARD` locks the dashboard out until the next restart, the same way it would lock out any
  other client.

The certificate and its key are written next to the CA (`.local/certs/web-dashboard.crt`
by default), so they follow `ca_path` rather than littering the working directory.

## What it shows

| Tab            | Contents                                                                                                                         |
|----------------|----------------------------------------------------------------------------------------------------------------------------------|
| Overview       | Uptime, listener, connection and request totals, pending writes, tables in memory, every connection open right now, and a storage roll-up naming the accounts that need attention. |
| Authorizations | Every authorized client: name, thumbprint, allowed accounts, admin flag. Authorize a thumbprint, add or remove accounts, revoke. |
| Certificates   | Issue a certificate signed by the server's CA, authorized in the same step, with its key downloadable once.                      |
| Accounts       | Every account with its file count, record count and size on disk; drill into an account's files and one file's statistics. Accounts and files can be created and dropped, durable files are tagged in the listing and the flag can be turned on or off, and the selected file's dictionary and indexes are listed and managed below. |

File statistics cover the record and dictionary counts, the indexes the file carries, the hash modulus and group
distribution, bytes on disk, the durability flag and whether the file is currently held in the server's cache. Record
counts come from each file's section metadata and the per-group counts from each group's trailer, so opening the view
does not load the file — a property asserted by a test, not merely intended.

### Health: what the numbers mean

The panel opens with a verdict rather than with numbers. Thirteen rows with no evaluation of any of them left the
reader to decide whether a four-megabyte largest group against a ninety-six-kilobyte smallest one was fine, and a
number nobody knows how to read is not information.

Every measure carries one of three verdicts and the threshold that produced it:

| Verdict            | Means                                                                                       |
|--------------------|---------------------------------------------------------------------------------------------|
| *healthy*          | Nothing to do.                                                                              |
| *watch*            | Not wrong now, and heading somewhere — a file about to rehash, an index nothing has queried. |
| *needs attention*  | Something is costing more than it should, and the detail says what to do about it.           |

**The page decides none of this.** The verdicts come from the database, which is also what the CLI prints, so the two
cannot disagree about where a line is and improving a threshold is one change in one place. `shared/health.ts` holds no
threshold at all: what is there is presentation — which verdict sorts first, what it is called, what it is coloured.

What is measured, and what to do about a bad one:

| Measure               | Bad means                                                              | Remedy                                                                                        |
|-----------------------|------------------------------------------------------------------------|-----------------------------------------------------------------------------------------------|
| **Storage format**    | Still the pre-hashfile flat file: every write rewrites the whole file, and a torn write reads back as missing records. | Converted on the next flush. Write to the file, or flush the database. |
| **Per-group checksums** | Written before the format appended a checksum trailer, so a truncated group reads back as fewer records rather than as an error. | Written on the next flush, as above. |
| **Group skew**        | The largest group holds several times the mean, so every write landing there rewrites all of it. | Keys sharing a prefix the hash does not separate are the usual cause; the fix is in how keys are chosen. Not judged below four records per group. |
| **Overweight groups** | More than a tenth of the groups hold over twice the mean — the hash is not spreading, rather than one group being unlucky. | As above. |
| **Load factor**       | Near or past the modulus' capacity: the next flush rehashes, rewriting every group. Or far below it: a directory of near-empty groups. | Nothing to do — both resolve on a flush. Worth knowing before it happens on a busy file. |
| **Bytes per record**  | Informational: what the disk is spent on, split between groups, indexes and metadata. | — |
| **Indexes**           | The worst verdict among the file's indexes, so a badly shaped one is visible from the file. | See the index table below. |

Beneath the verdicts is the **records-per-group distribution**, drawn as columns. Two extremes cannot show skew — a
smallest and a largest say nothing about whether a file is one long tail or one outlier, and it is the outlier that
costs. One column standing far out to the right is what an operator is looking for, and it is visible there and nowhere
in a table of numbers. The distribution is over the modulus rather than the group files: a group holding nothing has no
file, so counting only the files that exist would draw a piled-up file as a perfectly even one.

The layout table follows, with everything the panel showed before plus the spread and the headroom — how many records
until the modulus doubles, and how many until it halves.

**Finding a bad file without opening every file.** The file list and the account list each carry the same verdict as a
coloured pill, and the Overview tab has a **Storage** card naming the accounts that need attention. These are the
*cheap* verdict: section metadata and index `state` files only, no group trailers and no records, which is what a
listing can afford. The full measures arrive when a file is opened. A healthy row shows no pill at all — forty green
badges would hide the one that matters.

Durability is the one thing about a file the dashboard changes rather than reports: beside the statistics, **Make
durable** promotes the file so every write to it is flushed before being acknowledged, and **Buffer writes** returns it
to the database's flush policy. Promoting flushes what the file still had buffered, so no data is at risk while the flag
lands, and the file keeps its records either way. The change goes out as the ordinary `SET.FILE` command, so it is
refused unless the dashboard's own certificate is an admin one, and the page re-reads the flag from the database rather
than assuming the click took effect — a server running with `durable_writes = true` reports every file as durable
whatever the button asked for. See [Storage Engine](storage.md).

### Creating and dropping

Under the account list is a field that creates one. **Create account** makes an empty one (`CREATE.ACCOUNT`) and
**Create demo** makes the populated fixture (`CREATE.TEST.ACCOUNT`) — the same one the CLI creates, with `USERS` and
`PRODUCTS` files, their dictionaries, a multivalued field whose values go one level deeper still, a price carrying
an `MD2` conversion, and an [association group](data_structures.md#association-groups) over the `PRODUCTS` suppliers. It is the quickest way to have something real to point the file statistics and the dictionary
editor at. Each row carries a **Drop** (`DELETE.ACCOUNT`). The same pair sits under the file list: a name and a **Durable** tick create a file
(`CREATE.FILE`, durable from its first write), and each file has its own **Drop** (`DELETE.FILE`). All four are admin
commands, so a dashboard whose certificate is not an admin one is refused by the database and says so.

Both drops confirm first, naming what goes with them — an account drop names the number of files it takes. Two things
are deliberately not offered:

- **`SYSTEM`** is listed like any other account and can be asked for, but the database refuses to drop it: it holds the
  account registry and the authorized clients.
- **`DIR`** has no Drop. It is the account's own record of its files and their durability flags, so removing it through
  a file list would take the flags of every other file with it.

A created account comes with its `DIR` file and every file created in it joins that listing, so an account made here is
complete the moment it exists — there is nothing to finish off from the CLI.

Whatever changes, the page re-reads the lists straight afterwards rather than editing what is on screen — the account
listing is otherwise refreshed only every twenty seconds, which is a long time to look at a file that no longer exists.

### The dictionary of a file

Selecting a file lists its dictionary in full width beneath the three columns: the name a query uses, the attribute the
field sits at, the heading and width `LIST` lays it out with, the justification, the field it is associated with, and
the conversion. The raw definition — the whole truth about an entry, including anything at a position the table does
not name — is the title of the name cell.

**Add a dictionary entry** stores one (`SET.DICT`), suggesting the next free attribute number; **Edit** loads an
existing entry into the same form, because storing an entry under a name that already exists is what replacing it
means; **Delete** removes one (`DELETE` with `is_dict`) and leaves the field's data where it is. Selecting a different
file abandons an open edit rather than carrying it across.

**Associated with** names the controlling field this one's values pair with, making the two an
[association group](data_structures.md#association-groups) that `BY.EXP` explodes together. The box suggests the file's
other entries without being limited to them — a dictionary is written in some order, and naming a controller that does
not exist yet is allowed. **Pairs on** says which tier: each value, or each sub-value inside the controlling value. It
is offered only once a controlling field is named, because a tier without one is refused, and it fills in *each value*
by default rather than leaving the attribute blank. The table spells the second tier out and leaves the first implied,
since that is the one that adds nothing to read.

Nothing in the page judges an attribute number or a justification. `SET.DICT` does, and it fills in the defaults for
whatever the form left blank, so the page re-reads the dictionary after every change and shows what was actually
stored — a refusal appears in the banner in the database's own words.

### The indexes of an account

Above the per-file sections, and appearing as soon as an account is selected: every index in the account that is not
earning its keep, named by file and field, with the measures that say why.

This is the first gap an operator meets and the one the per-file table could never close. Nothing reported on an index
unless somebody opened the page for its file, so a database with forty files had no view saying which three were worth
attention — and a problem nobody is told about is a problem nobody finds. There are three columns of navigation before
a single index otherwise.

Only the exceptions are listed. A table of every index in the account would be the same wall of numbers one level
further out; what is wanted here is which file to open, and the file's own table has the rest. Each row is a way
through to that file. An account whose indexes are all fine says so, and one with none says what that costs.

It is read when an account is chosen and again after any change to an index, not on a poll: the listing walks every
file in the account, and a verdict that changes on a flush does not need a five-second refresh.

### The indexes of a file

Beneath the dictionary is the file's [secondary indexes](storage.md#secondary-indexes) — the fields on which
`WITH <field> = <value>` resolves through an index instead of reading every record — and everything an operator does to
them, in one place:

- **Statistics**, per index: the attribute it follows, the distinct values it holds, the record keys it indexes in
  total, its largest posting list, the lookups it has served since the server started, its size on disk and when it was
  last built. A file with no indexes says how many records every non-keyed selection reads instead.
- **A verdict** on each row, from the database, in the same three states the file's own measures use. The page used to
  hold its own thresholds here — a lookup narrowing to two records is "close to unique", a largest posting list of a
  quarter of the file is worth warning about — which were reasonable guesses the CLI did not share and which could only
  be improved by editing prose in a component. What is judged now: whether the index matches the records, how far a
  lookup actually narrows the file, whether one value covers so much of it that indexing that value buys nothing,
  whether anything has queried it at all, and how much of what it hands back survives the filter.
- **Values** opens one index's histogram (`INDEX.STATS`): the values holding the most record keys, largest first, each
  with its share of the file. This is what turns "this index is skewed" into "`STATUS = ACTIVE` is 91% of it". A stale
  index shows nothing here and says why — its postings do not describe the records, and an empty histogram would read
  as an empty index, which is a different and wrong thing to tell somebody.
- **Stop indexing**, on each value in that histogram, excludes it (`SET.INDEX.EXCLUDE`). The dashboard should not show
  someone which value is the problem and then send them to the CLI to do something about it. The confirmation says what
  the exclusion does to their queries, which is nothing: a query for an excluded value scans exactly as it would with
  no index at all, and returns the same records in the same order. What changes is that the longest posting list stops
  being rewritten on every write that touches it. Excluded values are listed above the histogram with a button that
  puts each one back. See [Excluded values](storage.md#excluded-values).
- **Creation**: **Index a field** offers the dictionary fields that are defined and not indexed yet — a list rather
  than a text box, because an index is on a field the page has just shown you, and `ID` is excluded as the record key
  that needs no index. Building it reads the file once (`CREATE.INDEX`).
- **Rebuild** derives an index from the records again (`REBUILD.INDEX`). An index that has fallen behind is tagged
  **stale** here and in the statistics panel above, and is rebuilt on its own when the file is next loaded — this is
  for doing it now.
- **Drop** removes an index and its section (`DELETE.INDEX`), after confirming, and says what that means: queries go
  back to scanning and the records stay.

Reading an index's values is its own request, made only when **Values** is pressed: the listing above it is read on
every navigation and stays cheap, while the histogram sorts the index's values. Excluding one rebuilds the index, so
the histogram is re-read afterwards as well as the list — the thing the operator was looking at when they pressed the
button is the thing that has just changed.

The three that change something are admin commands, so a dashboard whose certificate is not an admin one is refused by
the database and says so. Nothing here decides whether a field *can* be indexed — `CREATE.INDEX` does, and a refusal
appears in the banner in the database's own words. Every change re-reads both the index list and the file's statistics,
so the counts on screen are the ones that now hold rather than the ones the click was expected to produce.

## Security

The dashboard can authorize clients, hand out private keys, and drop an account and everything in it, so treat reaching
it as equivalent to holding an admin certificate.

- It **binds to `127.0.0.1` by default**. Point `web_addr` elsewhere only behind a reverse proxy that terminates TLS;
  the dashboard itself serves plain HTTP and says so at startup when it is bound to a non-loopback address.
- Every request needs the token, in the session cookie, an `Authorization: Bearer` header or a `?token=` parameter. The
  only exception is `/health`, which returns nothing but liveness. Tokens are compared in constant time.
- The page is served under `Content-Security-Policy: default-src 'none'` with only same-origin scripts and styles:
  nothing is fetched from anywhere else, and there is no inline script to smuggle anything into.
- Values from the database are written into the page as text, never as markup.

## HTTP API

Each endpoint is one protocol command. Responses are the protocol's own JSON; failures are
`{"error": "..."}` with a status code — `401` without a token, `502` when the database itself cannot be reached, and
otherwise the status that the protocol's own [error code](protocol.md#error-codes) maps to: `403` for a refusal on
privileges, `404` for something that is not there, `409` for something that is already there, `500` for a file the
database could not read or write, `503` for a command the server is not configured to answer, and `400` for everything
else. The code is what decides, not the wording of the message, so a reworded refusal keeps its status.

| Method   | Path                                   | Command                                                           |
|----------|----------------------------------------|-------------------------------------------------------------------|
| `GET`    | `/health`                              | none (liveness, no token required)                                |
| `GET`    | `/api/stats`                           | `SERVER.STATS`                                                    |
| `GET`    | `/api/clients`                         | `LIST.CONNS`                                                      |
| `POST`   | `/api/clients`                         | `AUTHORIZE.CONN`                                                  |
| `DELETE` | `/api/clients/{name}`                  | `DEAUTHORIZE.CONN`                                                |
| `POST`   | `/api/clients/{name}/accounts`         | `ADD.CLIENT.ACCOUNT` / `REMOVE.CLIENT.ACCOUNT` (`"remove": true`) |
| `POST`   | `/api/certificates`                    | `GENERATE.CERT`                                                   |
| `GET`    | `/api/accounts`                        | `LIST.ACCOUNTS`                                                   |
| `POST`   | `/api/accounts`                        | `CREATE.ACCOUNT`, or `CREATE.TEST.ACCOUNT` (`"demo": true`)       |
| `DELETE` | `/api/accounts/{account}`              | `DELETE.ACCOUNT`                                                  |
| `GET`    | `/api/accounts/{account}/files`        | `LIST.FILES`                                                      |
| `POST`   | `/api/accounts/{account}/files`        | `CREATE.FILE`                                                     |
| `GET`    | `/api/accounts/{account}/files/{file}` | `FILE.STATS`                                                      |
| `POST`   | `/api/accounts/{account}/files/{file}` | `SET.FILE`                                                        |
| `DELETE` | `/api/accounts/{account}/files/{file}` | `DELETE.FILE`                                                     |
| `GET`    | `/api/accounts/{account}/files/{file}/dictionary`        | `LIST.DICT`                                     |
| `POST`   | `/api/accounts/{account}/files/{file}/dictionary`        | `SET.DICT`                                      |
| `DELETE` | `/api/accounts/{account}/files/{file}/dictionary/{name}` | `DELETE` with `is_dict`                         |
| `GET`    | `/api/accounts/{account}/files/{file}/indexes`            | `LIST.INDEXES`                                  |
| `POST`   | `/api/accounts/{account}/files/{file}/indexes`            | `CREATE.INDEX`                                  |
| `POST`   | `/api/accounts/{account}/files/{file}/indexes/{field}/rebuild` | `REBUILD.INDEX`                           |
| `DELETE` | `/api/accounts/{account}/files/{file}/indexes/{field}`   | `DELETE.INDEX`                                  |

```sh
curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:8080/api/stats
curl -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
     -d '{"common_name":"reporting-bot","accounts":["SALES"]}' \
     http://127.0.0.1:8080/api/certificates
```

## Implementation

The server side lives in `crates/core/src/web/`:

| File           | Role                                                                           |
|----------------|--------------------------------------------------------------------------------|
| `mod.rs`       | Boot: issue and authorize the certificate, mint the token, accept connections. |
| `http.rs`      | The HTTP/1.1 subset the dashboard needs, with every read bounded.              |
| `client.rs`    | The pooled TLS connection to the protocol server.                              |
| `api.rs`       | Routes: one HTTP shape in, one protocol command out.                           |
| `assets/dist/` | The built front end, embedded in the binary at compile time.                   |

There is no HTTP framework: the protocol server was written the same way, and a dependency
tree larger than the database itself is a poor trade for a handful of routes.

### Front end

The page is a Vue 3 application using the Composition API, written as single-file components and built by Vite. It is
organised in **vertical slices**: each feature owns its views, its components, its composables, its API calls and its
types, in one directory.

```
crates/core/src/web/ui/src/
├── App.vue                  the shell: header, tabs, alert banner
├── features/
│   ├── index.ts             the registry - the one module that knows every slice
│   ├── types.ts             FeatureTab: all the shell knows about a feature
│   ├── overview/            ┐
│   │   ├── index.ts         │ public surface: the tab, plus the two header widgets
│   │   ├── api.ts           │ SERVER.STATS
│   │   ├── types.ts         │ ServerSnapshot, ConnectionSnapshot
│   │   ├── OverviewView.vue │
│   │   ├── components/      │ ServerLine, ServerControls, StatGrid, ConnectionsTable
│   │   └── composables/     ┘ useServerStats
│   ├── authorizations/      same shape: LIST.CONNS, AUTHORIZE.CONN, useClients, …
│   ├── certificates/        GENERATE.CERT, useCertificateIssuing, …
│   └── accounts/            LIST.ACCOUNTS, LIST.FILES, FILE.STATS, CREATE/DELETE.ACCOUNT,
│                            CREATE/DELETE.FILE, SET.FILE, LIST.DICT, SET.DICT,
│                            LIST.INDEXES, CREATE/REBUILD/DELETE.INDEX,
│                            useAccountBrowser, useFileDictionary, useFileIndexes, …
└── shared/                  the kernel every slice may use
    ├── api/client.ts        the transport: ApiError, call, record, pairs, keys
    ├── api/protocol.ts      the response envelope
    ├── composables/         usePolling, useAlerts
    ├── components/          StatCard, RolePill, PanelState, StatList
    ├── format.ts            durations, byte counts, thumbprints
    └── style.css
```

Adding a feature is a new directory plus one line in `features/index.ts`. Removing one is deleting a directory and that
line — nothing else in the tree refers to it.

**The rules**, asserted by `shared/architecture.test.ts` rather than left to good intentions:

1. A feature imports its own files and `@shared/...`. It never imports another feature; that is what a slice's
   `index.ts` is for, and only the registry may use it.
2. `shared/` never imports a feature. The kernel cannot depend on what is built on it.
3. Every slice has an `index.ts` and appears in the registry, so a feature cannot be half-wired and silently absent.

The test names the offending file and specifier when a rule is broken. Two path aliases,
`@shared/*` and `@features/*`, mean moving a file inside its own slice never rewrites a path outside it.

`shared/composables/usePolling.ts` is where the live-monitoring behaviour lives, and new watched views should be built
on it rather than on their own timers:

- Requests never overlap; the next tick is scheduled when the previous response lands, so a slow server slows the
  refresh rate instead of queueing requests.
- Polling stops while the browser tab is hidden, and refreshes immediately when it returns.
- A failed refresh keeps the last good data on screen, dimmed, with the reason beside it.
- A `401` stops the poll rather than retrying into a log full of refusals.

The overview slice's `useServerStats` shows the intended pattern for data more than one component needs: one
module-scope poller, shared, with consumers reference counted so it starts when the first component that wants it mounts
and stops when the last goes away. The shell never has to start a poll on behalf of a feature it otherwise knows nothing
about.

### Building it

The built bundle in `assets/dist/` is **committed**, so `cargo build` alone produces a working server and neither CI nor
the container image needs a node toolchain. Node is required only to change the interface:

```sh
make ui-build     # rebuild assets/dist - commit the result
make ui-test      # component and architecture tests (vitest + jsdom)
make ui-check     # type-check only
make ui-format    # Prettier
make ui-dev       # Vite dev server on :5173 with hot reload
```

Formatting is Prettier's (`ui/.prettierrc.json`), and CI fails on unformatted files. Indentation and line endings for
the rest of the repository come from `.editorconfig` at the root, which editors apply on their own.

**In a JetBrains IDE, two settings are worth a minute:**

1. Turn Prettier on for this project — *Settings → Languages & Frameworks → JavaScript → Prettier*, set the
   configuration to automatic and tick *Run on save*. Without it the IDE's own formatter and Prettier disagree on four
   details — brace spacing, the space in `<Component />`, continuation indent and attribute indent — and undo each other
   on every save until CI's `format:check` fails. Only the first is expressible in `.prettierrc.json`, so configuration
   alone cannot settle it.
2. Mark `crates/core/src/web/assets/dist` as *Excluded* (right-click → *Mark Directory as*). It is Vite output;
   reformatting it by hand makes a rebuild produce something different and fails the freshness check.

After `make ui-build`, rebuild the Rust binary too: the bundle is embedded at compile time, so a server built earlier
keeps serving the older copy.

For `make ui-dev`, start a database server first and open the dashboard URL it prints with the port changed to 5173 —
`http://127.0.0.1:5173/?token=...`. Vite proxies `/api` and
`/health` to `127.0.0.1:8080`, and in a dev build only, the page replays that token as a bearer header because the
production `HttpOnly` cookie belongs to a different origin. The production bundle contains no path that puts the token
anywhere script-readable.

Vite is configured to emit fixed filenames (`app.js`, `app.css`) rather than content-hashed ones, because `include_str!`
needs paths known at compile time. Nothing is lost: responses carry `Cache-Control: no-store`, so there is no cache to
bust.

### What keeps the bundle honest

A committed build artefact is only safe if it cannot silently drift from its sources:

- `.github/workflows/main.yml` has a `dashboard-bundle` job that runs the component tests, rebuilds the bundle and fails
  if `git diff` shows the committed copy differs.
- `cargo test` checks that the embedded page references exactly the assets the server serves, and that the bundle is a
  production build rather than a dev one — a dev build would need `unsafe-eval`, which the page's own policy refuses.
- `test/integration/test_web.py` fetches every `/dist/...` path the served page references and checks each returns 200
  with a usable content type.

`test/integration/test_web.py` also drives the real binaries end to end: the bootstrap, the token checks, every
endpoint, certificate issuing and revocation, and the certificate rotation across a restart.
