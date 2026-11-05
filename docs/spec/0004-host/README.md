# §host — The host

Chapter 0004 of the [swiss specification](../../SPEC.md).

The host is what every plugin shares and nothing more: the security boundary, config and
credentials, registration, lifecycle and run accounting. `swiss-core` holds the platform
primitives, `swiss-host` the mechanism, the root crate the composition and the CLI.

**The host keeps cross-cutting mechanism only.** MCP tool selection, SQL browsing, SSH rule
logic, cron parsing and any plugin's cache or front-end state never live here. Replacing the
old big `AppContext` with a giant `Services` object would be the same mistake renamed.

**Parts.** The sections live in five files beside this one, in reading order:

1. [Plugins, routes and the wire](01-plugins.md) — the plugin contract, its lifecycle, the route trees, the wire rules
2. [Access, config and credentials](02-access.md) — the loopback boundary, the session, tokens, the config store, references, the vault
3. [What the plugins share](03-shared.md) — groups, actions and runs, log storage, capability seats, the memory reading
4. [The command line](04-command-line.md) — the commands, and configuring a machine from a manifest
5. [Running it](05-running.md) — the desktop window, the daemon, production and the test instance
