# Managed Agents

Thermite's reference triage agent as [Claude Managed Agents](https://platform.claude.com/docs/en/managed-agents/overview)
resources, kept in sync with `ant apply`. The agent is a plain MCP client of `/mcp`; nothing here
is required to run Thermite, and Thermite itself never calls a model.

```
agents/triage.md            the agent: model, the thermite MCP server, system prompt
environments/cloud.yaml     a cloud container whose egress is limited to MCP servers
vaults/thermite-demo.yaml   the vault that holds the Thermite API key (the key is added by hand)
deployments/triage.md       hourly: one session that drains the queue
claude-lock.json            written by `ant apply`; maps each file to its resource id — commit it
```

## Apply

From this directory (the lockfile lives here):

```sh
ant apply --dry-run -v agents environments vaults deployments   # the plan
ant apply agents environments vaults deployments                # create / update
```

Then add the API key once (mint an `oat_` key in the Thermite dashboard; the vault file has the
command). The deployment fires on its own from then on. To try it without waiting:

```sh
ant beta:deployments run --deployment-id "$(jq -r '.resources["./deployments/triage.md"].id' claude-lock.json)"
```

Pointing it at another instance means changing the URL in `agents/triage.md` and the credential's
`mcp_server_url` together: credentials are matched to servers by URL.

The schedule is the only trigger. Reacting to an alert instead would mean something holding an
Anthropic key calling `POST /v1/deployments/{id}/run` when Thermite's alert webhook fires; Managed
Agents has no inbound trigger of its own.
