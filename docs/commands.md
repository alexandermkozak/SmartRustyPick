### Application Modes

SmartRustyPick can be run in two modes:

#### CLI Mode (Default)

When run without any flags, SmartRustyPick opens an interactive CLI.

- **Auto-login**: If you launch the application from a directory associated with an account (as defined in
  `CREATE.ACCOUNT`), it will automatically log into that account.
- **Background Service**: If SSL certificates are provided in `config.toml`, the database service starts automatically
  in the background, allowing remote clients to connect to the session.

#### Headless Mode

When run with the `--headless` flag, the application starts the database service without the CLI.

- **Usage**: `./SmartRustyPick --headless`
- **Requirement**: Requires `cert_path`, `key_path`, and `ca_path` to be configured in `config.toml`.

#### MCP Mode

SmartRustyPick also includes a Model Context Protocol (MCP) server for integration with AI agents.

- **Usage**: `make mcp-run` (after `make mcp-setup`)
- **Documentation**: See [MCP Server README](../mcp/README.md) for detailed tool descriptions and configuration.

### Data Organization

SmartRustyPick organizes data into **Accounts**. Each account is a collection of files (tables).
When you start the application, if not auto-logged, you will be prompted to log into an account.

### Commands

SmartRustyPick CLI commands are divided into two categories:

- [Administration Commands](admin_commands.md) - For system management, account creation, and server security.
- [General Use Commands](general_commands.md) - For day-to-day data operations and queries.
