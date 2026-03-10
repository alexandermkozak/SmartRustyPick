# SmartRustyPicksrc

## Purpose

The goal of this project is to provide a simple PICK-like multivalue database implementation useful for personal
projects that is done entirely through vibe-coding.

This is not only a personal project/tool, but a proof of concept and an exploration of the usefulness of AI agents as
someone who sees value in the use of AI, but struggles to believe claims from companies and influencers that say they no
longer write or read production level code.

To that end, this is being developed in Rust, which is a language I'd like to learn, have spent 2 hours following
introductory tutorials for, but otherwise have no experience or knowledge of. The goal is that I will be able to
understand the code enough to know when refactoring or other changes to code may be appropriate, but that I do not know
if the solutions implemented are necessarily correct or efficient down to the line.

With its natural support for safety and embedded unit testing, Rust seemed like the perfect choice for the project.

For development I am using Jetbrains RustRover with their Junie agent. So far, the primary model used has been the
default in Junie, Gemini 3 Flash.

## Introduction
SmartRustyPick is a CLI tool for interacting with a MultiValue-inspired database. It supports hierarchical data
organization through **Accounts**, data records with multiple fields, values and sub-values, dictionary definitions for
field formatting, and complex select operations.

## Features

- **Account-level Organization**: Multiple accounts can exist within a single system, providing data isolation.
- **Hierarchical Records**: Support for Field Mark (FM), Value Mark (VM), and Sub-Value Mark (SVM).
- **Dictionary Support**: Define field indices and formatting/conversions (Dates, Numbers).
- **Active Select Lists**: Perform queries and refine them through sequential `SELECT` commands.
- **Remote Access**: TCP SSL server with certificate authentication and CRUD protocol.
- **Persistent Configuration**: Customize your environment (e.g., preferred editor, SSL certificates, server address).
- **Headless Mode**: Run the database as a background service without a CLI.
- **Smart Login**: Automatic CLI account login based on the current working directory.
- **MCP Server Support**: Integrated Model Context Protocol (MCP) server for database interaction by other AI agents.

## Documentation

For more information, see the following documentation:

- [Data Structures](docs/data_structures.md) - Learn how records and dictionaries are structured.
- [Commands](docs/commands.md) - See a full list of available CLI commands and modes.
- [Remote Protocol](docs/protocol.md) - Details on the TCP/SSL remote protocol.
- [MCP Server](mcp/README.md) - Usage instructions for the Model Context Protocol server.
- [AI Agents](agents.md) - Documentation on the role and contributions of AI agents in this project.

## Configuration

The application can be configured via a `config.toml` file in the root directory.
Currently supported settings:

- `editor`: The command to launch for the `EDIT` command (default: `nano`).
- `server_port`: The default port for the SSL server (default: 8443).
- `server_addr`: The address the server should bind to (default: `127.0.0.1`).
- `cert_path`: Path to the server SSL certificate.
- `key_path`: Path to the server SSL private key.
- `ca_path`: Path to the CA certificate for client authentication.

If SSL certificate paths are provided in `config.toml`, the database service will automatically start in the background
when the CLI is launched.

## Quick Start

1. Compile the project with `cargo build --release`.
2. Run as a CLI: `./target/release/SmartRustyPick`.
   - The CLI will automatically log into an account if the current directory is associated with one.
3. Run as a headless service: `./target/release/SmartRustyPick --headless`.
   - Requires SSL settings in `config.toml`.
4. Type `HELP` in the CLI to see all commands.

## MCP Server

This project includes a Model Context Protocol (MCP) server located in the `mcp/` directory. This allows other AI agents
to interact with the database using standardized tools.

### Running the MCP Server

1. Setup the environment: `make mcp-setup`
2. Run the server: `make mcp-run`

For detailed configuration and tool descriptions, see [mcp/README.md](mcp/README.md).
