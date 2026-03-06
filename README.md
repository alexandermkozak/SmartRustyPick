# SmartRustyPick

SmartRustyPick is a CLI tool for interacting with a MultiValue-inspired database. It supports hierarchical data records, dictionary definitions for field formatting, and complex select operations.

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

## Documentation

For more information, see the following documentation:

- [Data Structures](docs/data_structures.md) - Learn how records and dictionaries are structured.
- [Commands](docs/commands.md) - See a full list of available CLI commands.

## Configuration

The application can be configured via a `config.toml` file in the root directory.
Currently supported settings:

- `editor`: The command to launch for the `EDIT` command (default: `nano`).

## Quick Start

1. Compile the project with `cargo build --release`.
2. Run the executable: `./target/release/SmartRustyPick`.
3. Type `HELP` in the CLI to see all commands.
