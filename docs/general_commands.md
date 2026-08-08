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

- **Usage**: `LIST [DICT] [<table> [<fields>...] [BY|BY.DSND <field> ...]]`
- **Example**: `LIST USERS First.Name Last.Name`
- **Example**: `LIST PRODUCTS BY PRICE`
- **Example**: `LIST PRODUCTS BY.DSND PRICE`
- **Sorting**: See [Sorting](#sorting) below.

#### SELECT

Create or refine an active select list based on field criteria.

- **Usage**:
  `SELECT [DICT] <table> [WITH <field> <op> <value> [AND/OR <field> <op> <value> ...]] [BY|BY.DSND <field> ...]`
- **Operators**: `=`, `#` (not equal), `<`, `>`, `<=`, `>=`, `[` (ends with), `]` (starts with), `[]` (contains)
- **Logical Operators**: `AND`, `OR`
- **Example**: `SELECT USERS WITH First.Name = "Ted" AND Last.Name = "Smith"`
- **Sorting**: See [Sorting](#sorting) below.

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

- **Usage**: `CREATE.FILE <name>`
- **Example**: `CREATE.FILE ORDERS`

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
