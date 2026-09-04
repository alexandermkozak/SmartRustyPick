### Data Structures

SmartRustyPick uses a hierarchical data structure inspired by MultiValue databases.

#### Record
A `Record` is the top-level data unit, uniquely identified by a key within a table.
- Internal representation: `Vec<Field>`
- Separator: `FM` (Field Mark, `\xFE` or `254`)
- Display/Edit representation: Newlines or `^`

#### Field
A `Field` is a component of a `Record`.
- Internal representation: `Vec<Value>`
- Separator: `VM` (Value Mark, `\xFD` or `253`)
- Display/Edit representation: `]`

#### Value
A `Value` is a component of a `Field`, allowing for multi-valued fields.
- Internal representation: `Vec<SubValue>` (Sub-values)
- Separator: `SVM` (Sub-Value Mark, `\xFC` or `252`)
- Display/Edit representation: `\`

#### Sub-Value
A `Sub-Value` is the most granular unit of data, stored as **raw bytes** (`Vec<u8>`).

Bytes rather than a `String`, because a record has always been a byte container on disk.
Reading one used to pass every sub-value through a lossy UTF-8 conversion, which replaced
any byte that was not valid UTF-8 with `U+FFFD` and lost the original: a write that
reported success and read back as something else. Text is now a *view* of a sub-value,
taken where a caller asks for text, rather than the way it is stored.

Every byte value survives storage and retrieval untouched, **except the three marks**.
`FM` (`0xFE`), `VM` (`0xFD`) and `SVM` (`0xFC`) are the structure of a record, so a mark
inside a sub-value is indistinguishable from the separator it is, and reading the record
back splits the value in two. That is the MultiValue data model rather than a defect, and
it is why content that may contain arbitrary bytes belongs in a blob referenced by the
record rather than inlined into one.

Over the remote protocol, a sub-value that is not valid UTF-8 travels as
`{"$base64": "..."}` — see [Protocol](protocol.md#values-that-are-not-text). A sub-value
that *is* valid UTF-8 travels as a plain JSON string, exactly as before.

#### Dictionary Items
Dictionary items are special records stored in the `dict` section of a table. They define how data in the `data` section is interpreted.
- **Field 1**: Field index (1-based).
- **Field 2**: Display Heading (used in LIST output).
- **Field 3**: Justification (`L` for Left, `R` for Right).
- **Field 4**: Display Width (cosmetic constraint for LIST output).
- **Field 5**: Association — the controlling field this entry's values pair with (optional). See
  [Association groups](#association-groups).
- **Field 6**: Association depth — `V` (value for value, the default) or `S` (sub-value for
  sub-value). Only read when field 5 names a controller.
- **Field 8**: Conversion Code (optional).
  - `D4-`: Date with 4-digit year (e.g., 03-21-2026).
  - `D2/`: Date with 2-digit year (e.g., 03/21/26).
  - `MR<n>`: Number with `<n>` decimal places (e.g., `MR2` converts `12345` to `123.45`).
  - `MD<n>`: Number with `<n>` decimal places (e.g., `MD2` converts `12345` to `123.45`).

A dictionary entry is also what an index is defined on: `CREATE.INDEX <file> <field>` names an entry, and the index
follows the attribute that entry points at. Moving an entry to a different attribute therefore makes its index stale,
which is detected and repaired rather than silently wrong — see [Storage Engine](storage.md#secondary-indexes).

#### Multivalues in queries and output

A selection matches a field if **any** of its values - or, where a value has them, any of its sub-values - satisfies the
condition. `WITH ACCOUNTS = "TEST"` finds a record whose `ACCOUNTS` field holds `TEST]PAYROLL`, and does not need to
know that the field is multivalued.

Output, by default, keeps a field whole: `LIST` renders it as one cell with the values joined by `]` and any sub-values
by `\`, and the remote protocol serialises it as a JSON array of its values. Which value satisfied the query is not
part of that answer.

`BY.EXP` asks for it. It gives each value of a named field its own output row, and, when a criterion names the same
field, only the values that satisfied it. A select list built that way remembers the position each row came from - the
value index, and the sub-value index when the match went that deep - so a later `LIST` of the same file still shows
exactly those values. Over the remote protocol the same positions come back alongside the results.

The clause is documented, with its rules and examples, under
[Exploding multivalues](general_commands.md#exploding-multivalues); the wire form is under
[Exploded results](protocol.md#exploded-results).

#### Association groups

Some multivalued fields are only meaningful together. A `$CLIENTS` record with
`ACCOUNTS` holding `TEST]PAYROLL]LAB` and `ACCT.CODES` holding `T-1]P-7]L-3` means three
accounts, each with its own code — not three accounts and, separately, three codes. Exploding
one of them ought to explode the other in lockstep.

Nothing in the values says so, and nothing can: three values beside three values could mean
three rows or nine. The dictionary says so. **Attribute 5 of a dependent names its
controller**, and the fields sharing a controller are an *association group* — PICK's
correlated multivalued attributes.

```
ACCOUNTS     2^ACCOUNTS^L^12
ACCT.CODES   3^CODE^L^8^ACCOUNTS^V
ACCT.NOTES   4^NOTE^L^10^ACCOUNTS^S
```

The relationship is recorded on the dependent rather than as a list on the controller, so a
field is in at most one group by construction and there is no list to fall out of step with
the entries it names. A controller carries nothing: it is found by the entries that name it.
A dependent may name a dependent, and the chain is followed to its head, so a group can be
built up in tiers; a chain that runs in a circle forms no group rather than hanging.

**Two tiers.** Attribute 6 says which one a dependent pairs on:

- `V` (the default) — value for value. Row *n* is value *n* of every `V` member.
- `S` — sub-value for sub-value, *inside* the controlling value. A value that has sub-values
  at the second tier becomes one row per sub-value, and the `V` members repeat down them.

A group is not a constraint. Members may be ragged — three accounts beside two codes — and
the rows come from the longest of them, the short ones showing empty. Nothing is dropped
because a sibling ran out of values, and nothing rejects a write that leaves a group uneven.

What a group changes is `BY.EXP`: naming any member explodes all of them, one row's position
is read against every member, and several members may be named in one clause. The rules and
examples are under [Exploding multivalues](general_commands.md#exploding-multivalues).

#### Conversions (ICONV / OCONV)

SmartRustyPick supports automatic data conversion between internal storage format and external display format:

- **OCONV (Output Conversion)**: Applied when reading data (e.g., `12345` -> `123.45`).
- **ICONV (Input Conversion)**: Applied when writing structured data (e.g., `123.45` -> `12345`).

Conversions apply to each value and sub-value of a field, not to the field as a whole, so an `MD2` column holding
`120000]250` reads as `1200.00]2.50`.

#### Database Layout

The database is stored in the `db_storage` directory, organized by account:

- `db_storage/<account>/<table>/data.hf/`: A directory containing data records in a hashed layout.
    - `meta`: Metadata file (version, modulus, record count).
    - `g<hex>`: Group files containing hashed records.
- `db_storage/<account>/<table>/dict`: A flat file containing dictionary records.
- `db_storage/<account>/<table>/index.<field>.hf/`: A [secondary index](storage.md#secondary-indexes) on a dictionary
  field, in the same hashed layout as the records. Its keys are the indexed values and each record holds the keys
  carrying that value; a `state` file names the field, the attribute it resolved to and the data version it matches.
- `$SAVEDLISTS`: A special table used to store named select lists.

**Record Framing**: Both group files and the dictionary use the frame encoding:
`[key_len u64 LE][key][data_len u64 LE][record bytes]`.

**Automatic Migration**: Tables in the old flat `data` file format are automatically converted to the `data.hf/` layout
on the first flush after being read.

For more details on the storage engine, hashing, and write buffering, see [Storage Engine](storage.md).
