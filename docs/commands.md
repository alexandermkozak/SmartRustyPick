### Commands

SmartRustyPick CLI supports the following commands:

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
- **Usage**: `LIST [DICT] [<table> [<fields>...]]`
- **Example**: `LIST USERS First.Name Last.Name`

#### SELECT
Create or refine an active select list based on field criteria.
- **Usage**: `SELECT [DICT] <table> [WITH <field> <op> <value>]`
- **Operators**: `=`, `#` (not equal), `<`, `>`, `<=`, `>=`, `[` (ends with), `]` (starts with), `[]` (contains)
- **Example**: `SELECT USERS WITH First.Name = "Ted"`

#### EDIT
Edit a record using an external editor defined by the `$EDITOR` environment variable.
- **Usage**: `EDIT [DICT] <table> <key>`
- **Example**: `EDIT USERS 1`

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
