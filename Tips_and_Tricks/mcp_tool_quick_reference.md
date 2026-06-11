# 🧭 MCP Tool Quick Reference

> **Use this card when designing or reviewing an MCP tool definition.**
>
> Goal: make the tool easy for models to choose correctly, easy for clients to render safely, and accurate to the latest MCP Tool schema.

---

## 1. MCP Tool Fields at a Glance

| Field | Required? | What It Does |
|---|---|---|
| `name` | Yes | Stable machine identifier |
| `title` | No | Human-readable display name |
| `description` | No | Explains purpose, intended use, and non-use |
| `inputSchema` | Yes | JSON Schema for tool inputs |
| `outputSchema` | No | JSON Schema for structured outputs |
| `annotations` | No | Behavioral hints for clients and models |
| `execution` | No | Execution-related metadata |
| `icons` | No | UI display metadata |
| `_meta` | No | Implementation-specific extension space |

> **Remember:** `version`, `type: "tool"`, `auth`, `metadata`, and embedded `prompts` blocks are **not** standard MCP Tool schema fields.

---

## 2. Start With These Three Questions

Before writing the schema, answer:

1. **What should this tool do?**
2. **When should it be called?**
3. **When should it not be called?**

If you cannot answer those clearly, the model will have trouble routing to the tool correctly.

---

## 3. Description Checklist

A strong `description` usually includes:

- the tool’s primary job
- clear trigger intents
- at least one negative rule
- a short example if helpful

### Weak

```json
{ "description": "Handles prompts." }
```

### Better

```json
{
  "description": "Create, retrieve, update, or delete saved prompt artifacts. Use when the user explicitly wants to save, fetch, rename, modify, or remove a stored prompt. Do not use for brainstorming, drafting, or general advice."
}
```

---

## 4. `inputSchema` Checklist

Good `inputSchema` design usually means:

- every meaningful property has a `description`
- bounded strings use `minLength` / `maxLength`
- closed sets use `enum`
- object inputs use `type: "object"`
- unexpected fields are blocked with `additionalProperties: false` when helpful
- required fields are declared explicitly

### Weak

```json
{
  "type": "object",
  "properties": {
    "title": { "type": "string" }
  }
}
```

### Better

```json
{
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "title": {
      "type": "string",
      "minLength": 3,
      "maxLength": 256,
      "description": "Short, outcome-oriented title."
    }
  },
  "required": ["title"]
}
```

---

## 5. `outputSchema` Checklist

Use `outputSchema` when structured results matter.

It helps with:

- predictable downstream reasoning
- cleaner tool chaining
- easier client rendering
- reduced dependence on free-form text parsing

### Good starter shape

```json
{
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "status": {
      "type": "string",
      "description": "Result status."
    },
    "data": {
      "type": "object",
      "description": "Structured result payload."
    },
    "errors": {
      "type": "array",
      "items": { "type": "string" },
      "description": "Error messages, if any."
    }
  },
  "required": ["status"]
}
```

---

## 6. `annotations` Cheat Sheet

| Annotation | Use It For |
|---|---|
| `readOnlyHint` | Fetch/search/list/inspect tools |
| `destructiveHint` | Delete/overwrite/send/commit/execute tools |
| `idempotentHint` | Tools that are safe to retry with the same input |
| `openWorldHint` | Tools that touch external or changing systems |

### Example: read-only tool

```json
{
  "annotations": {
    "readOnlyHint": true,
    "destructiveHint": false,
    "idempotentHint": true,
    "openWorldHint": false
  }
}
```

### Example: open-world tool

```json
{
  "annotations": {
    "readOnlyHint": true,
    "destructiveHint": false,
    "idempotentHint": false,
    "openWorldHint": true
  }
}
```

> **Important:** annotations are hints, not guarantees.

---

## 7. Trust Boundary Reminder

If `openWorldHint: true`, assume the tool may:

- retrieve untrusted text
- pull in public internet content
- surface external SaaS or third-party data
- increase risk when chained with write-capable tools

### Good practice

- treat outputs as potentially untrusted
- do not treat retrieved text as trusted instructions
- be more careful when chaining open-world tools with tools that mutate state or access private data

---

## 8. Simple vs. Strict Schemas

### Start simple when:

- the audience is beginner-friendly
- the tool has one stable input shape
- mistakes are low-cost
- runtime clarification is acceptable

### Go stricter when:

- the tool writes or deletes data
- actions require clearly different payloads
- mistakes are expensive
- you want more correctness enforced in schema

Useful stricter-schema tools:

- `additionalProperties: false`
- `enum`
- `const`
- `oneOf`
- `anyOf`
- `pattern`
- `format`

---

## 9. Spec vs. Heuristic

### MCP spec fields

These belong in the Tool object:

- `name`
- `title`
- `description`
- `inputSchema`
- `outputSchema`
- `annotations`
- `execution`
- `icons`
- `_meta`

### Good heuristics, but not spec requirements

These are often helpful, but they are conventions:

- including a few user → tool examples
- using CRUD verbs for persistence tools
- asking one clarifying question when input is incomplete
- adding a negative rule to the description
- keeping titles short and outcome-oriented

### Runtime concerns, not tool-schema fields

These usually live outside the Tool object:

- authentication
- authorization
- retries
- approvals
- policy controls
- orchestration logic

---

## 10. Common Mistakes

Avoid these:

- using `input_schema` instead of `inputSchema`
- using `output_schema` instead of `outputSchema`
- documenting `version` as if it were a standard MCP tool field
- putting `auth` in the Tool object as if it were schema-standard
- relying on a vague `description`
- leaving properties undocumented
- using overly loose object schemas when bad keys would be harmful
- forgetting that open-world outputs may be untrusted

---

## 11. Beginner Starter Example

```json
{
  "name": "artifact_crud",
  "title": "Artifact CRUD",
  "description": "Create, retrieve, update, or delete persistent artifacts. Use when the user explicitly wants to save, fetch, rename, modify, or remove a stored item. Do not use for brainstorming, drafting, or general advice.",
  "inputSchema": {
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "type": "object",
    "additionalProperties": false,
    "properties": {
      "action": {
        "type": "string",
        "enum": ["create", "get", "update", "delete"],
        "description": "Operation to perform."
      },
      "payload": {
        "type": "object",
        "description": "Action-specific data.",
        "additionalProperties": false,
        "properties": {
          "id": {
            "type": "string",
            "description": "Artifact identifier."
          },
          "title": {
            "type": "string",
            "minLength": 3,
            "maxLength": 256,
            "description": "Short, outcome-oriented title."
          },
          "body": {
            "type": "string",
            "minLength": 1,
            "maxLength": 8192,
            "description": "Artifact body text."
          }
        }
      }
    },
    "required": ["action"]
  },
  "annotations": {
    "readOnlyHint": false,
    "destructiveHint": true,
    "idempotentHint": false,
    "openWorldHint": false
  }
}
```

---

## 12. Fast QA Checklist

- [ ] Tool uses `inputSchema` and `outputSchema`
- [ ] Description says when to use and when not to use the tool
- [ ] Important properties have descriptions
- [ ] Closed sets use `enum`
- [ ] Strictness level matches the tool’s risk
- [ ] Annotations reflect actual behavior
- [ ] Open-world outputs are treated as potentially untrusted
- [ ] Non-schema conventions are not presented as MCP requirements

---

> **Rule of thumb:** be strict about the spec, clear about intent, and pragmatic about heuristics.
