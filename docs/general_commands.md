### General Use Commands

These commands are used for day-to-day data operations within a specific account.

#### LOGTO

Switch the current context to a different account.

- **Usage**: `LOGTO <account name>`
- **Example**: `LOGTO SALES`
- **Note**: When switching to an account that lacks a `DIR` file, you will be prompted to create and populate it.

#### LIST.FILES

List all files in the current account. This command reads from the `DIR` file.

- **Usage**: `LIST.FILES`
- **Example**: `LIST.FILES`

#### SET

Store a record in the database.

- **Usage**: `SET [DICT] <table> <key> <data>`
- **Example**: `SET USERS 1 Ted^Smith]123-4567`

#### GET

Retrieve a record by its key or via an active SELECT list.

- **Usage**: `GET [DICT] <table> [<key>]`
- **Example**: `GET USERS 1`

#### DELETE

Remove a record from the database.

- **Usage**: `DELETE [DICT] <table> [<key>]`
- **Example**: `DELETE USERS 1`

#### LIST

List tables, keys, or records with formatted fields.

- **Usage**:
  `LIST [DICT] [<table> [<fields>...] [WITH <field> <op> <value> ...] [BY|BY.DSND <field> ...] [BY.EXP <field> [<op> <value>]]]`
- **Example**: `LIST USERS First.Name Last.Name`
- **Example**: `LIST PRODUCTS BY PRICE`
- **Example**: `LIST PRODUCTS BY.DSND PRICE`
- **Example**: `LIST USERS WITH Last.Name = "Smith" First.Name Last.Name`
- **Selection**: `LIST` takes the same `WITH` clause `SELECT` does. Column names may sit on either side of it. Without
  one, `LIST` consumes the active select list if there is one for the same file, as it always has.
- **Sorting**: See [Sorting](#sorting) below.
- **Multivalue**: See [Exploding multivalues](#exploding-multivalues) below.

#### SELECT

Create or refine an active select list based on field criteria.

- **Usage**:
  `SELECT [DICT] <table> [WITH <field> <op> <value> [AND/OR <field> <op> <value> ...]] [BY|BY.DSND <field> ...]`
- **Operators**: `=`, `#` (not equal), `<`, `>`, `<=`, `>=`, `[` (ends with), `]` (starts with), `[]` (contains)
- **Logical Operators**: `AND`, `OR`
- **Example**: `SELECT USERS WITH First.Name = "Ted" AND Last.Name = "Smith"`
- **Sorting**: See [Sorting](#sorting) below.
- **Multivalue**: See [Exploding multivalues](#exploding-multivalues) below.

#### Exploding multivalues

A field can hold several values, and by default `LIST` shows all of them in one cell, separated by `]`. `BY.EXP` gives
each value its own row instead, so a multivalued field reads like a list rather than like one long string.

- `BY.EXP <field>` - one row per value of that field
- `BY.EXP <field> <op> <value>` - the same, filtered: only the values that satisfy the criterion get a row

The second form is shorthand. `BY.EXP ACCOUNTS = "TEST"` and `BY.EXP ACCOUNTS WITH ACCOUNTS = "TEST"` ask the same
question. The operator set is closed, so a bare column name after the field is still read as a column:
`LIST USERS BY.EXP ROLES NAME` explodes `ROLES` and shows the `NAME` column.

Given a `$CLIENTS` record `WEB` whose `ACCOUNTS` field holds `TEST]PAYROLL]TESTLAB`:

```
LIST $CLIENTS BY.EXP ACCOUNTS = "[TEST]" ACCOUNTS

ID         ACCOUNTS
---------- --------------------
WEB        TEST
WEB        TESTLAB
```

The select list remembers the positions, so the two-step form works too - the `LIST` that follows shows the same rows:

```
SELECT $CLIENTS BY.EXP ACCOUNTS = "TEST"
LIST $CLIENTS ACCOUNTS
```

`SAVE-LIST` and `GET-LIST` carry the positions with the list, so a saved exploded list restores as one.

The rules in detail:

- Only the named field explodes. Every other column repeats the record's whole field down the rows.
- Which records appear is still the query's decision. A record kept by a condition on some other field appears once,
  unexploded. A record whose exploded field is empty does too.
- With a criterion, the unit is the deepest thing that matched: a value, or a sub-value when the field has them. Two
  conditions on the same field union their positions, so `WITH ROLES = "DEV" OR ROLES = "ADMIN"` gives a row for each.
- `BY.EXP` does not sort. Rows come out in record-key order, and within a record in value order. Add `BY <field>` to
  order them - and when that field is the exploded one, it sorts on each row's own value rather than on the whole
  joined field.
- Only one `BY.EXP` field may be given.

Commands that act on records rather than on values - `GET`, `DELETE`, `CT` - take each record from an exploded list
once, not once per matching value.

#### Sorting

`LIST` and `SELECT` accept any number of sort operators. Sorts are applied from left to right, so the first operator is
the primary sort key, the second breaks ties, and so on. Records that compare equal on every key fall back to record ID
order. Sort operators and column names are order-agnostic and may be freely interleaved.

- `BY <field>` - ascending sort
- `BY.DSND <field>` - descending sort

The special field name `ID` sorts on the record key. Values that are numeric on both sides are compared numerically,
otherwise they are compared as text.

- **Example**: `LIST PRODUCTS BY PRICE` - list products by ascending price
- **Example**: `LIST PRODUCTS BY.DSND PRICE` - list products by descending price
- **Example**: `SELECT PRODUCTS WITH DESC = "[new]" BY PRICE BY.DSND CREATE.DATE` - select products whose description
  contains "new", ordered by ascending price, then by descending create date
- **Example**: `LIST PRODUCTS BY.DSND DESC DESC PRICE` and `LIST PRODUCTS DESC PRICE BY.DSND DESC` are equivalent - both
  show the `DESC` and `PRICE` columns sorted by descending `DESC`
- **Example**: `LIST USERS BY.EXP ROLES ROLES BY ROLES` - one row per role, ordered by the role on that row

#### EDIT

Edit a record using an external editor.

- **Usage**: `EDIT [DICT] <table> <key>`
- **Example**: `EDIT USERS 1`
- **Configuration**: The editor can be configured in `config.toml` in the application root (e.g., `editor = "nano"`). If
  not set, it defaults to the `$EDITOR` environment variable, then to `nano`.

#### CT (Copy-To-Terminal)

Print record contents with numbered fields.

- **Usage**: `CT [DICT] <table> [<key>]`
- **Example**: `CT DICT USERS D1`

#### SAVE

Flush all changes from memory to the disk.

- **Usage**: `SAVE`

#### SAVE-LIST

Save the current active SELECT list.

- **Usage**: `SAVE-LIST <name>`
- **Example**: `SAVE-LIST TED_LIST`

#### GET-LIST

Retrieve a previously saved SELECT list.

- **Usage**: `GET-LIST <name>`
- **Example**: `GET-LIST TED_LIST`

#### CREATE.FILE

Create a new table (both data and dictionary sections).

The optional `DURABLE` flag marks the file as mission critical: every write to it is flushed to disk before it is
acknowledged, while the rest of the database keeps buffering writes. The flag is stored as the `DURABLE` attribute of
the file's `DIR`
entry - see [Storage Engine](storage.md).

- **Usage**: `CREATE.FILE <name> [DURABLE]`
- **Example**: `CREATE.FILE ORDERS`
- **Example**: `CREATE.FILE LEDGER DURABLE`

#### DELETE.FILE

Delete a table (both data and dictionary sections).

- **Usage**: `DELETE.FILE <name>`
- **Example**: `DELETE.FILE OLD_DATA`

#### HELP

Show the help message.

- **Usage**: `HELP`

#### EXIT / QUIT

Exit the SmartRustyPick CLI.

- **Usage**: `EXIT` or `QUIT`
