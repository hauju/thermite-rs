---
# yaml-language-server: $schema=https://platform.claude.com/schemas/ant/beta/deployment.json
name: Thermite triage (demo)
description: Hourly sweep of demo.thermite.rs's triage queue. Thermite's lease model makes overlapping runs safe.
agent: ../agents/triage.md
environment_id: ../environments/cloud.yaml
# `ant apply` does not resolve a path here, so this is the id from claude-lock.json for
# ./vaults/thermite-demo.yaml.
vault_ids:
  - vlt_011CfoZoE1Q3JmHsUNCqXpAJ
# An empty run costs a few cents; a run that finds work is bounded by the budget below.
schedule:
  type: cron
  expression: "0 * * * *"
  timezone: UTC
# Hard cap per fired session, in cents. A run that finds nothing costs a few cents; this is the
# ceiling for a run that finds a lot.
budget:
  type: limit
  max_list_cost:
    amount: "300"
    currency: USD
---

Drain the triage queue once: claim, diagnose and post an analysis for every pending issue, then stop.
