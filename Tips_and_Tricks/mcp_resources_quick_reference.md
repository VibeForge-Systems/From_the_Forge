# 🧭 MCP Resources Quick Reference

> **Use this card when designing or reviewing MCP resources.**
>
> Goal: make resources discoverable, predictable, and safe to consume without presenting heuristics as protocol requirements.
>
> Companion cards: `mcp_tool_quick_reference.md`, `mcp_prompts_quick_reference.md` • Full guide: `mcp_tool_tips.md`

---

## 1. MCP Resource Fields at a Glance

| Field | Required? | What It Does |
|---|---|---|
| `uri` | Yes | Stable resource identifier |
| `name` | Yes | Human-readable resource name |
| `title` | No | Optional display title |
| `description` | No | Purpose and usage guidance |
| `mimeType` | No | Media type hint for clients |
| `size` | No | Optional byte-size hint |
| `annotations` | No | Behavioral hints for clients and models |
| `_meta` | No | Implementation-specific extension space |

> **Remember:** resources describe retrievable context. Access control, policy, and runtime orchestration are implementation concerns.

---

## 2. Start With These Three Questions

Before publishing a resource, answer:

1. **What should this resource help the model or user understand?**
2. **When should this resource be selected?**
3. **When should this resource be skipped?**

If this is unclear, clients and models will overfetch or miss relevant context.

---

## 3. Description Checklist

A strong `description` usually includes:

- what the resource contains
- when it is useful
- at least one boundary (what it does *not* cover)

### Weak

```json
{ "description": "Project info." }
```

### Better

```json
{
  "description": "Release runbook for deployment rollouts and rollback steps. Use for operational release preparation and incident recovery. Do not use as the source of feature requirements."
}
```

---

## 4. URI and Naming Tips

- keep `uri` stable; changing it breaks references
- use readable, domain-specific `name` values
- keep naming patterns consistent across related resources
- avoid encoding volatile details in the `uri` when versioning can be modeled separately

> Good convention: treat URI stability as a contract, even when content evolves.

---

## 5. MIME Type and Size Guidance

- set `mimeType` when known to improve rendering and parsing
- set `size` when cheaply available so clients can budget context usage
- do not treat `size` as an authorization or trust signal

### Example

```json
{
  "uri": "forge://runbooks/release-rollout",
  "name": "Release Rollout Runbook",
  "mimeType": "text/markdown",
  "size": 12483
}
```

---

## 6. `annotations` Cheat Sheet

| Annotation | Use It For |
|---|---|
| `audience` | Hinting intended consumer scope |
| `priority` | Hinting relative retrieval importance |
| `lastVerified` | Surfacing freshness metadata |

> **Important:** annotation keys are implementation conventions unless defined by your runtime contract.

---

## 7. Trust Boundary Reminder

Even internal-looking resources can contain stale, incomplete, or untrusted text.

Good practice:

- treat resource content as input, not authority
- cross-check high-risk decisions against source-of-truth systems
- be cautious when resource content can trigger state-changing actions

---

## 8. Spec vs. Heuristic

### MCP resource fields

Use documented resource fields such as:

- `uri`
- `name`
- `title`
- `description`
- `mimeType`
- `size`
- `annotations`
- `_meta`

### Helpful heuristics, not protocol requirements

- adding “when to use” examples in `description`
- using consistent URI namespaces by domain
- tagging freshness in annotations

### Runtime concerns outside resource objects

- authentication and authorization
- policy enforcement
- fetch retries and caching strategy
- approval and governance workflows

---

## 9. Fast QA Checklist

- [ ] Resource has stable `uri` and clear `name`
- [ ] Description explains use and scope boundaries
- [ ] `mimeType` is provided when known
- [ ] Metadata is helpful but not presented as protocol-mandated
- [ ] Content is treated as potentially untrusted input
- [ ] Runtime policy/auth concerns are documented separately

---

> **Rule of thumb:** keep resource contracts stable, descriptions practical, and boundaries explicit.
