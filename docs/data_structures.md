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
- **Field 7**: Conversion Code (optional).
  - `D4-`: Date with 4-digit year (e.g., 03-21-2026).
  - `D2/`: Date with 2-digit year (e.g., 03/21/26).
  - `MR<n>`: Number with `<n>` decimal places (e.g., `MR2` converts `12345` to `123.45`).

#### Database Layout
The database is stored in the `db_storage` directory:
- `db_storage/<table_name>/data`: Contains data records.
- `db_storage/<table_name>/dict`: Contains dictionary records.
- `$SAVEDLISTS`: A special table used to store named select lists.
