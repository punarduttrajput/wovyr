<!--
File: docs/11-cli/commands.md
Document ID: CLI-003
-->

# CLI Command Reference

**Document ID:** CLI-003  
**File Path:** `docs/11-cli/commands.md`  
**Version:** 1.0.0  
**Status:** Draft  
**Owner:** AI Platform Team  
**Last Updated:** 2026-06-27

---

# 1. Purpose

This document is the reference for `apex` commands. Command groups mirror the [Platform API](../09-api/index.md) resource model, plus local-development and operations commands.

---

# 2. Synopsis

```text
apex [global flags] <group> <command> [args] [flags]
```

Global flags: `--server`, `--tenant`, `--project`, `--context`, `--output|-o`,
`--quiet`, `--yes`, `--log`. See [Configuration](configuration.md).

---

# 3. Top-Level Commands

| Command | Purpose |
|---------|---------|
| `apex login` / `logout` | Authenticate / sign out |
| `apex init` | Scaffold a new project workspace |
| `apex context` | Manage contexts/profiles |
| `apex config` | Get/set config values |
| `apex doctor` | Diagnose environment + connectivity |
| `apex version` | Show CLI and target API versions |
| `apex upgrade` | Self-update |
| `apex completion` | Generate shell completion |

---

# 4. Agents — `apex agents`

Maps to the [Agents API](../09-api/agents.md).

```bash
apex agents list [--status published]
apex agents get <id|name>
apex agents create -f agent.yaml
apex agents update <id> -f agent.yaml
apex agents publish <id>
apex agents run <id|name> --input '{"message":"hi"}' [--stream] [--session <id>]
apex agents runs get <run_id>
apex agents runs cancel <run_id>
apex agents runs logs <run_id> [--follow]
```

`run --stream` renders the live step/tool/model stream
([Agents API §6](../09-api/agents.md#6-run-lifecycle--streaming)).

---

# 5. Workflows — `apex workflows`

Maps to the [Workflows API](../09-api/workflows.md).

```bash
apex workflows list
apex workflows get <id>
apex workflows create -f workflow.yaml
apex workflows validate -f workflow.yaml        # compile-check the DSL
apex workflows publish <id>
apex workflows run <id> --input @input.json [--version 2.1.0]
apex workflows executions list [--status running]
apex workflows executions get <exe_id> [--watch]
apex workflows executions cancel <exe_id>
apex workflows executions signal <exe_id> --event PaymentReceived --payload @p.json
apex workflows tasks complete <task_id> --decision approved
```

`validate` runs the DSL compiler/validator
([DSL §24](../03-workflow-engine/workflow-dsl.md#24-validation-rules)); `--watch`
streams execution state transitions.

---

# 6. Memory — `apex memory`

Maps to the [Memory (Management) API](../09-api/memory.md).

```bash
apex memory namespaces list
apex memory namespaces create -f ns.yaml
apex memory query "refund window" [--scope project] [--limit 10]
apex memory get <record_id>
apex memory put -f record.yaml
apex memory purge --filter 'type=conversation,age>90d' [--hard]
apex memory reindex <namespace>                 # async operation
apex memory export <namespace> --out memories.jsonl
```

Query results include the [score breakdown](../06-memory-engine/ranking.md#9-output).

---

# 7. Tools — `apex tools`

Maps to the [Tools API](../09-api/tools.md).

```bash
apex tools list [--category network]
apex tools get <name>
apex tools schema <name>                         # print input/output schema
apex tools enable <name> --project <p>
apex tools disable <name> --project <p>
apex tools invoke <name> --input @input.json [--stream]
```

`invoke` proxies to the [Tool Runtime](../07-tool-runtime/execution-api.md) with the
caller's authorization.

---

# 8. Plugins — `apex plugin(s)`

Authoring (`apex plugin`, see [Plugin API §8](../08-plugin-sdk/plugin-api.md#8-developer-workflow--cli))
and management (`apex plugins`, see [Plugins API](../09-api/plugins.md)).

```bash
# authoring / local
apex plugin new <name> --kind tool,workflow_activity
apex plugin build
apex plugin test
apex plugin sign --key <key>
apex plugin publish --registry <url>

# management (remote)
apex plugins search <query> [--verified]
apex plugins install <name>@<version>
apex plugins list
apex plugins enable <id> | disable <id>
apex plugins upgrade <id> | rollback <id>
apex plugins grants add <id> --project <p> --permission net:egress:api.github.com
apex plugins uninstall <id>
```

Install verifies signature/provenance and surfaces the
[permission grant flow](../08-plugin-sdk/permissions.md#5-grant-flow).

---

# 9. Projects, Users, Auth — `apex projects` / `apex users`

Maps to [Projects API](../09-api/projects.md) and [Users API](../09-api/users.md).

```bash
apex projects list | get <id> | create -f project.yaml
apex projects quota get <id> | set <id> --llm-cost-per-day 250
apex projects members add <id> --user <uid> --role editor

apex users list | invite alex@example.com --role viewer
apex users me                                   # current identity + scopes
apex apikeys create --subject <svc> --scope workflows:run --expires 2027-01-01
apex apikeys revoke <key_id>
```

Admin commands require the corresponding
[admin scopes](../09-api/authentication.md#8-roles).

---

# 10. Local Development

```bash
apex init my-project                 # scaffold workspace (agents/, workflows/, plugins/)
apex workflow run --local -f wf.yaml --input @in.json    # embedded runtime, no server
apex agents run --local -f agent.yaml --input '{...}'
apex dev                             # run an all-in-one local platform for testing
```

`--local` uses the embedded runtime ([Configuration §9](configuration.md#9-local-mode-settings)),
ideal for authoring and tests before publishing to a server.

---

# 11. Operations — `apex deploy` / `apex logs`

```bash
apex deploy --target kubernetes -f deployment.yaml      # see Deployment section
apex logs <run_id|exe_id> [--follow]
apex doctor                                              # connectivity + version + toolchain
apex status                                              # platform/service health
```

`deploy` targets are specified in the planned
[Deployment](../SUMMARY.md) section.

---

# 12. Help & Discovery

```bash
apex help
apex <group> --help
apex <group> <command> --help
```

Every command documents its flags, examples, and required scopes in `--help`.

---

# 13. Output & Automation

All commands honor `--output json|yaml|table` and stable
[exit codes](configuration.md#8-exit-codes), making them composable in scripts and
CI (see [Examples](examples.md)).

---

# 14. Dependencies

- [`09-api/index.md`](../09-api/index.md)
- [`08-plugin-sdk/plugin-api.md`](../08-plugin-sdk/plugin-api.md)
- [`07-tool-runtime/execution-api.md`](../07-tool-runtime/execution-api.md)

---

# 15. Related Documents

- [`11-cli/configuration.md`](configuration.md)
- [`11-cli/examples.md`](examples.md)

---

# 16. Revision History

| Version | Date | Description |
|---------|------|-------------|
| 1.0.0 | 2026-06-27 | Initial CLI Command Reference |
