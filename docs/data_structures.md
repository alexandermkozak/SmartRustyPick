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
- Internal representation: `Vec<String>` (Sub-values)
- Separator: `SVM` (Sub-Value Mark, `\xFC` or `252`)
- Display/Edit representation: `\`

#### Sub-Value
A `Sub-Value` is the most granular unit of data, stored as a `String`.

#### Dictionary Items
Dictionary items are special records stored in the `dict` section of a table. They define how data in the `data` section is interpreted.
- **Field 1**: Field index (1-based).
- **Field 2**: Display Heading (used in LIST output).
- **Field 3**: Justification (`L` for Left, `R` for Right).
- **Field 4**: Display Width (cosmetic constraint for LIST output).
- **Field 8**: Conversion Code (optional).
  - `D4-`: Date with 4-digit year (e.g., 03-21-2026).
  - `D2/`: Date with 2-digit year (e.g., 03/21/26).
  - `MR<n>`: Number with `<n>` decimal places (e.g., `MR2` converts `12345` to `123.45`).
  - `MD<n>`: Number with `<n>` decimal places (e.g., `MD2` converts `12345` to `123.45`).

#### Conversions (ICONV / OCONV)

SmartRustyPick supports automatic data conversion between internal storage format and external display format:

- **OCONV (Output Conversion)**: Applied when reading data (e.g., `12345` -> `123.45`).
- **ICONV (Input Conversion)**: Applied when writing structured data (e.g., `123.45` -> `12345`).

#### Database Layout

The database is stored in the `db_storage` directory, organized by account:

- `db_storage/<account>/<table>/data.hf/`: A directory containing data records in a hashed layout.
    - `meta`: Metadata file (version, modulus, record count).
    - `g<hex>`: Group files containing hashed records.
- `db_storage/<account>/<table>/dict`: A flat file containing dictionary records.
- `$SAVEDLISTS`: A special table used to store named select lists.

**Record Framing**: Both group files and the dictionary use the frame encoding:
`[key_len u64 LE][key][data_len u64 LE][record bytes]`.

**Automatic Migration**: Tables in the old flat `data` file format are automatically converted to the `data.hf/` layout
on the first flush after being read.

For more details on the storage engine, hashing, and write buffering, see [Storage Engine](storage.md).
