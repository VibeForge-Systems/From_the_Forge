# ⚒️ MCP Tool Tips — The VibeForge Developer's Field Guide

> **TL;DR** — An LLM cannot infer your tool’s real purpose from the name alone. Every MCP tool field is a signal. The clearer your schema and guidance, the more reliably the model can choose, parameterize, and safely use the tool.

This guide is a **best-practices companion** to the **Model Context Protocol (MCP) Tool schema**. It focuses on how to make tools easier for models to use correctly — while staying accurate to the latest MCP specification.

---

## Why This Matters

Imagine you ship a tool named `process-item`.

No meaningful description.  
No parameter descriptions.  
No examples.  
No hints about whether it is read-only, destructive, retry-safe, or dependent on outside systems.

The model sees a black box.

It starts guessing:

- maybe this is a save tool
- maybe it mutates data
- maybe it is safe to retry
- maybe it is just for formatting
- maybe it should be used for a brainstorming request

Sometimes it guesses right. Often it doesn’t.

**Poorly described tools don’t just reduce accuracy — they reduce trust in the entire system.**

The good news: the fixes are straightforward.  
The better news: the MCP schema already gives you several built-in places to provide those signals.

---

## Part 1 — What an MCP Tool Actually Looks Like

A modern MCP Tool is centered on these schema-defined fields:

| Field | Purpose | Notes |
|---|---|---|
| `name` | Stable identifier for the tool | Required |
| `title` | Human-readable display name | Optional but useful |
| `description` | High-level purpose and usage guidance | Optional, but extremely important |
| `inputSchema` | JSON Schema for tool inputs | Required |
| `outputSchema` | JSON Schema for structured output | Optional |
| `annotations` | Behavioral hints for clients/models | Optional |
| `execution` | Execution-related properties | Optional |
| `icons` | UI display metadata | Optional |
| `_meta` | Extension space for implementation-specific metadata | Optional |

> **Important:** Some things teams often include in internal tool definitions — such as `version`, `type: "tool"`, embedded `auth`, or custom prompt blocks like `normalize` / `validate` / `infer` — are **not standard MCP Tool schema fields**. If you use them, treat them as implementation conventions, not MCP-spec fields.

---

## Part 2 — Three Layers of Good MCP Tool Design

Think of MCP tool quality in three layers:

### Layer 1 — Schema-Valid MCP Fields

These are the fields the MCP Tool schema actually supports:

- `name`
- `title`
- `description`
- `inputSchema`
- `outputSchema`
- `annotations`
- `execution`
- `icons`
- `_meta`

These fields are your first and best chance to help both clients and models understand the tool.

---

### Layer 2 — Tool Authoring Conventions

These are not part of the MCP schema itself, but they are still valuable:

- example user → tool mappings
- rules about when not to call the tool
- internal normalization logic
- clarification policies
- validation playbooks
- action preference order

These can live in:

- your developer documentation
- `_meta`
- server-side logic
- client orchestration prompts
- internal testing guides

They are useful — just don’t present them as official MCP Tool fields.

---

### Layer 3 — Runtime / System Concerns

These usually belong outside the Tool object itself:

- authentication
- authorization
- rate limits
- retries
- human approval flows
- external network controls
- server policy enforcement

These are critical to production systems, but they are not modeled as standard top-level MCP Tool fields.

---

## Part 3 — The Highest-Leverage Field: `description`

Your tool’s `description` is still one of the most important signals you can provide.

A strong description answers three questions:

1. What does this tool do?
2. When should it be used?
3. When should it **not** be used?

### Weak vs strong descriptions

```json
{ "description": "Handles prompts." }
```

That tells the model almost nothing.

```json
{
  "description": "Create, retrieve, update, or delete saved prompt artifacts. Use this tool when the user explicitly wants to persist, fetch, rename, modify, or remove a stored prompt. Do not use it for brainstorming, drafting, or general advice."
}
```

That gives the model a much better routing contract.

### Recommended description pattern

A strong MCP tool description usually contains:

- the tool’s primary job
- explicit trigger intents
- at least one negative rule
- brief examples if they fit naturally

### Example

```json
{
  "description": "Create, retrieve, update, or delete persistent artifacts. Use when the user explicitly wants to save, fetch, rename, modify, or remove a stored item. Do not use for brainstorming, drafting, or general advice. Examples: 'Save this prompt', 'Get my last artifact', 'Rename item 123'."
}
```

> **Practical rule:** negative guidance is one of the easiest ways to reduce false-positive tool calls.

---

## Part 4 — `inputSchema`: Where Precision Starts

`inputSchema` is required by MCP and should be treated as more than validation.

It is also **guidance**.

A model reads your schema to infer:

- which fields matter
- what “good” input looks like
- how specific values should be
- which inputs are required

### Weak schema

```json
{
  "inputSchema": {
    "type": "object",
    "properties": {
      "title": { "type": "string" }
    }
  }
}
```

This is valid, but not very helpful.

### Stronger schema

```json
{
  "inputSchema": {
    "type": "object",
    "properties": {
      "title": {
        "type": "string",
        "minLength": 3,
        "maxLength": 256,
        "description": "Short, outcome-oriented title. Example: 'Glass Receipt: cost per agent'."
      }
    },
    "required": ["title"]
  }
}
```

This gives both validation and shape guidance.

### Recommended `inputSchema` practices

- add `description` to every meaningful property
- use `enum` for closed action sets
- use `minLength` / `maxLength` for bounded strings
- use `required` deliberately
- prefer explicit structure over generic blobs
- use `additionalProperties: false` when you want stricter contracts
- use richer JSON Schema constructs where helpful

---

## Part 5 — `outputSchema`: Make Results Predictable

`outputSchema` is optional in MCP, but very useful.

It tells clients and models what structured output the tool returns. If your tool returns structured content, define it clearly.

### Good reasons to include `outputSchema`

- predictable downstream reasoning
- easier client rendering
- cleaner chaining between tools
- less dependence on parsing free-form text

### Example

```json
{
  "outputSchema": {
    "type": "object",
    "properties": {
      "status": {
        "type": "string",
        "description": "Result status, such as 'success' or 'error'."
      },
      "data": {
        "type": "object",
        "description": "Structured result payload for successful operations."
      },
      "errors": {
        "type": "array",
        "items": { "type": "string" },
        "description": "Error messages, if any."
      }
    },
    "required": ["status"]
  }
}
```

### Guidance

- if you define `outputSchema`, return output that actually conforms to it
- keep success and error shapes understandable
- avoid making free-form text the only useful output when structured output is possible

---

## Part 6 — `annotations`: Native MCP Tool Guidance

This is one of the most underused and highest-value parts of the MCP Tool schema.

`annotations` are built-in hints that help clients and models reason about tool behavior.

### MCP tool annotations

| Annotation | Meaning |
|---|---|
| `title` | Alternate human-readable title hint |
| `readOnlyHint` | Tool does not modify its environment |
| `destructiveHint` | Tool may perform destructive updates |
| `idempotentHint` | Repeated calls with same input have no additional effect |
| `openWorldHint` | Tool interacts with external or changing world state |

### Why these matter

These annotations help answer questions like:

- Is this safe to call casually?
- Should the client require confirmation?
- Is retrying safe?
- Could this tool interact with outside systems?
- Should output be treated as crossing a trust boundary?

### Examples

#### Read-only lookup tool

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

#### Delete tool

```json
{
  "annotations": {
    "readOnlyHint": false,
    "destructiveHint": true,
    "idempotentHint": false,
    "openWorldHint": false
  }
}
```

#### Web search tool

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

### Practical rules

- set `readOnlyHint: true` for list/search/fetch/inspect tools
- set `destructiveHint: true` for delete/overwrite/send/commit/execute actions with meaningful side effects
- set `idempotentHint: true` only when retries are truly safe
- set `openWorldHint: true` for tools that depend on external systems, remote APIs, the internet, or changing real-world state

> **Important:** annotations are hints, not guarantees. The MCP guidance is explicit that clients should treat annotations as untrusted unless they come from a trusted server, and the default posture is intentionally cautious when annotations are absent.

---

## Part 7 — Trust Boundaries and `openWorldHint`

`openWorldHint` deserves special attention.

Unlike the other common annotation booleans, `openWorldHint` is not just about whether a tool is safe to call. It is also about **what kind of data may come back** and whether that data may originate from an external or changing environment.

### Why this matters

A tool marked `openWorldHint: true` may:

- return content from the public internet
- surface data from external SaaS systems
- bring untrusted text into the model context
- combine with other tools in ways that increase session risk

### Practical trust-boundary guidance

When a tool is open-world:

- assume its outputs may contain untrusted content
- avoid treating retrieved text as trusted instructions
- consider stricter approval or review policies when mixed with write-capable tools
- be especially careful when chaining open-world tools with tools that can mutate state, send messages, or access private data

### Recommended author guidance

Use `openWorldHint: true` when the tool reaches beyond a closed domain such as:

- the public internet
- third-party APIs
- remote user-generated content
- external enterprise systems outside the local trust boundary

Use `openWorldHint: false` when the tool operates within a clearly bounded, closed domain such as:

- a local static knowledge base
- an internal deterministic dataset
- a fixed repository snapshot
- a constrained local computation

> **Design note:** `openWorldHint` is best understood as a trust-boundary and provenance signal, not just a “web tool” flag.

---

## Part 8 — `title`, `name`, and Display Clarity

Use `name` for stable identity.  
Use `title` for clear human presentation.

### Good pattern

```json
{
  "name": "artifact_crud",
  "title": "Artifact CRUD"
}
```

### Bad pattern

```json
{
  "name": "tool1"
}
```

### Guidance

- keep `name` stable, machine-oriented, and concise
- keep `title` readable and UI-friendly
- do not rely on the tool name alone to communicate purpose

---

## Part 9 — `_meta`: Use for Extensions, Not Core Meaning

`_meta` is the schema-approved place for implementation-specific metadata.

Use it when you need extra data that is useful to your own clients or systems but is **not standard MCP Tool schema**.

### Good uses for `_meta`

- internal authoring examples
- testing hints
- implementation-specific policies
- UI metadata not covered elsewhere
- compatibility data for a known client

### Example

```json
{
  "_meta": {
    "vibeforgeGuidance": {
      "examples": [
        "Save this as a new prompt -> create",
        "Get my last prompt -> get",
        "Rename prompt 123 to Cost Audit -> update"
      ],
      "clarificationPolicy": "Ask one concise question when required fields are missing."
    }
  }
}
```

### Do not misuse `_meta`

Do not use `_meta` as an excuse to hide a weak schema.

If something belongs in:

- `description`
- `inputSchema`
- `outputSchema`
- `annotations`

put it there first.

---

## Part 10 — `execution` and `icons`

These fields are often overlooked.

### `execution`

Use `execution` for execution-related metadata when your implementation supports it. This is part of the MCP Tool schema and may matter for task-oriented or asynchronous execution environments.

### `icons`

Use `icons` when you want the tool to render more clearly in clients with UI support.

These fields are not usually the primary source of model guidance, but they are still part of the modern schema and worth considering when building polished tools.

---

## Part 11 — What MCP Does *Not* Standardize Inside the Tool Object

These are common and useful patterns, but they are **not standard top-level MCP Tool fields**.

### Not MCP Tool schema fields

- `version`
- `type: "tool"`
- `auth`
- `metadata`
- `prompts.normalize`
- `prompts.validate`
- `prompts.infer`

### Where those ideas belong instead

| Concern | Better home |
|---|---|
| Tool versioning | documentation, release metadata, `_meta`, or server versioning |
| auth details | transport, server config, auth layer |
| normalization logic | server/client orchestration |
| validation policy | JSON Schema + runtime validation |
| ambiguity policy | assistant prompt, orchestrator, or server logic |
| examples for internal routing | docs or `_meta` |

These ideas are still useful. Just don’t present them as part of the official MCP Tool schema.

---

## Part 12 — Recommended Tool Guidance Beyond the Schema

Even though MCP does not standardize embedded `normalize`, `validate`, and `infer` blocks in the Tool object, those are still excellent design concepts.

Use them as **tool-authoring practices**:

### 1. Normalize user intent

Map loose user language into your tool’s expected structure.

Examples:

- `"Save this as a new prompt"` → `action = "create"`
- `"Get my last saved prompt"` → `action = "get"`
- `"Rename prompt 123 to 'Cost Audit'"` → `action = "update"`

### 2. Validate required information

Before calling the tool, ensure the chosen action has the required fields.

If not, ask **one concise clarifying question**.

### 3. Infer carefully under ambiguity

When the user is ambiguous:

- prefer strong explicit signals like IDs and action verbs
- ask one clarifying question if confidence is low
- avoid repeated multi-question interrogations

These are great orchestration behaviors — just keep them outside the official Tool schema unless you are storing them in `_meta` for your own systems.

---

## Part 13 — Stricter JSON Schema Patterns for Higher-Reliability Tools

If you want tools to behave more predictably, it often helps to make the schema stricter.

The MCP spec supports modern JSON Schema patterns, and the current guidance defaults schemas to **JSON Schema 2020-12** when `$schema` is not explicitly provided. That means you can use richer JSON Schema patterns than just `type`, `properties`, and `required`.

### Useful strictness patterns

#### 1. `additionalProperties: false`

Use this when you want to prevent invented or misspelled fields.

```json
{
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "id": { "type": "string" },
    "title": { "type": "string" }
  }
}
```

This is especially useful when the model might otherwise hallucinate extra keys.

#### 2. `enum`

Use enums for closed action sets or state values.

```json
{
  "type": "string",
  "enum": ["create", "get", "update", "delete"]
}
```

This reduces ambiguity and encourages stable tool calling.

#### 3. `oneOf` for action-specific shapes

If different actions require meaningfully different payloads, `oneOf` can make that explicit.

```json
{
  "type": "object",
  "oneOf": [
    {
      "properties": {
        "action": { "const": "get" },
        "payload": {
          "type": "object",
          "required": ["id"],
          "properties": {
            "id": { "type": "string" }
          },
          "additionalProperties": false
        }
      },
      "required": ["action", "payload"]
    },
    {
      "properties": {
        "action": { "const": "create" },
        "payload": {
          "type": "object",
          "required": ["title", "body"],
          "properties": {
            "title": { "type": "string" },
            "body": { "type": "string" }
          },
          "additionalProperties": false
        }
      },
      "required": ["action", "payload"]
    }
  ]
}
```

This can be more precise than a single permissive `payload` object.

#### 4. `const` for discriminators

Use `const` inside branches when you want each schema variant to declare exactly one action or mode.

#### 5. `pattern`, `format`, `minItems`, and bounds

Use these to communicate expected shapes for identifiers, emails, URLs, lists, and bounded text.

### Practical guidance

- use stricter schemas when the tool contract is stable and mistakes are expensive
- use looser schemas when exploratory input is expected
- do not add complexity for its own sake — schema strictness should serve usability and safety

> **Recommendation:** for write tools, transactional tools, and tools with expensive side effects, stricter schemas are often worth the extra effort.

---

## Part 14 — A Simple Starter Example

If your audience includes beginners, it helps to show a minimal but still spec-accurate example first.

This version keeps the schema approachable:

- one object-shaped `inputSchema`
- one object-shaped `outputSchema`
- clear descriptions
- useful annotations
- no advanced branching constructs

This is often a good starting point for:

- first MCP tools
- internal tools with simple inputs
- tools where action-specific validation can be handled in runtime logic

```json
{
  "name": "vibeforge_crud_tool",
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
        "description": "Action-specific data. For create: title, body, tags. For update: id plus changed fields. For get or delete: id.",
        "additionalProperties": false,
        "properties": {
          "id": {
            "type": "string",
            "description": "Artifact identifier. Typically used for get, update, and delete."
          },
          "title": {
            "type": "string",
            "minLength": 3,
            "maxLength": 256,
            "description": "Short, outcome-oriented title. Example: 'Glass Receipt: cost per agent'."
          },
          "body": {
            "type": "string",
            "minLength": 1,
            "maxLength": 8192,
            "description": "Artifact body text. State the outcome, constraints, and expected result clearly."
          },
          "tags": {
            "type": "array",
            "items": {
              "type": "string"
            },
            "description": "Categorical tags for discovery and routing."
          }
        }
      },
      "requesterContext": {
        "type": "object",
        "description": "Optional caller context to support implementation-specific inference."
      }
    },
    "required": ["action"]
  },
  "outputSchema": {
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "type": "object",
    "additionalProperties": false,
    "properties": {
      "status": {
        "type": "string",
        "description": "Result status, such as 'success' or 'error'."
      },
      "data": {
        "type": "object",
        "description": "Structured result payload for successful operations."
      },
      "errors": {
        "type": "array",
        "items": {
          "type": "string"
        },
        "description": "List of error messages, if any."
      }
    },
    "required": ["status"]
  },
  "annotations": {
    "readOnlyHint": false,
    "destructiveHint": true,
    "idempotentHint": false,
    "openWorldHint": false
  }
}
```

### Why this example is beginner-friendly

- it uses only the most common MCP Tool fields
- it avoids `oneOf`, `const`, and `anyOf`
- it is still fully spec-aligned
- it leaves room for runtime clarification when the input is incomplete

> **Good default:** start with a simple schema when the tool contract is easy to explain and mistakes are low-cost.

---

## Part 15 — A Stricter Reference Tool

When you want stronger validation and clearer action-specific shapes, you can move to a stricter schema.

This version is often a better fit for:

- write tools
- tools with expensive or irreversible side effects
- tools where different actions require clearly different payload shapes
- systems where you want more correctness enforced in schema instead of runtime logic

```json
{
  "name": "vibeforge_crud_tool",
  "title": "Artifact CRUD",
  "description": "Create, retrieve, update, or delete persistent artifacts. Use when the user explicitly wants to save, fetch, rename, modify, or remove a stored item. Do not use for brainstorming, drafting, or general advice.",
  "inputSchema": {
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "type": "object",
    "allOf": [
      {
        "oneOf": [
          {
            "properties": {
              "action": {
                "const": "get",
                "description": "Retrieve an existing artifact."
              },
              "payload": {
                "type": "object",
                "description": "Lookup payload.",
                "additionalProperties": false,
                "properties": {
                  "id": {
                    "type": "string",
                    "description": "Artifact identifier."
                  }
                },
                "required": ["id"]
              }
            },
            "required": ["action", "payload"]
          },
          {
            "properties": {
              "action": {
                "const": "delete",
                "description": "Delete an existing artifact."
              },
              "payload": {
                "type": "object",
                "description": "Delete payload.",
                "additionalProperties": false,
                "properties": {
                  "id": {
                    "type": "string",
                    "description": "Artifact identifier."
                  }
                },
                "required": ["id"]
              }
            },
            "required": ["action", "payload"]
          },
          {
            "properties": {
              "action": {
                "const": "create",
                "description": "Create a new artifact."
              },
              "payload": {
                "type": "object",
                "description": "Create payload.",
                "additionalProperties": false,
                "properties": {
                  "title": {
                    "type": "string",
                    "minLength": 3,
                    "maxLength": 256,
                    "description": "Short, outcome-oriented title. Example: 'Glass Receipt: cost per agent'."
                  },
                  "body": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 8192,
                    "description": "Artifact body text. State the outcome, constraints, and expected result clearly."
                  },
                  "tags": {
                    "type": "array",
                    "items": {
                      "type": "string"
                    },
                    "minItems": 0,
                    "description": "Categorical tags for discovery and routing."
                  }
                },
                "required": ["title", "body"]
              }
            },
            "required": ["action", "payload"]
          },
          {
            "properties": {
              "action": {
                "const": "update",
                "description": "Update an existing artifact."
              },
              "payload": {
                "type": "object",
                "description": "Update payload.",
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
                    "description": "Updated title."
                  },
                  "body": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 8192,
                    "description": "Updated artifact body text."
                  },
                  "tags": {
                    "type": "array",
                    "items": {
                      "type": "string"
                    },
                    "description": "Updated tags."
                  }
                },
                "required": ["id"],
                "anyOf": [
                  { "required": ["title"] },
                  { "required": ["body"] },
                  { "required": ["tags"] }
                ]
              }
            },
            "required": ["action", "payload"]
          }
        ]
      }
    ],
    "properties": {
      "requesterContext": {
        "type": "object",
        "description": "Optional caller context to support implementation-specific inference."
      }
    }
  },
  "outputSchema": {
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "type": "object",
    "additionalProperties": false,
    "properties": {
      "status": {
        "type": "string",
        "description": "Result status, such as 'success' or 'error'."
      },
      "data": {
        "type": "object",
        "description": "Structured result payload for successful operations."
      },
      "errors": {
        "type": "array",
        "items": {
          "type": "string"
        },
        "description": "List of error messages, if any."
      }
    },
    "required": ["status"]
  },
  "annotations": {
    "readOnlyHint": false,
    "destructiveHint": true,
    "idempotentHint": false,
    "openWorldHint": false
  },
  "_meta": {
    "vibeforgeGuidance": {
      "examples": [
        "User: 'Save this as a new prompt' -> { action: 'create', payload: { title: '...', body: '...' } }",
        "User: 'Get my last prompt' -> { action: 'get' }",
        "User: 'Rename prompt 123 to \"Cost Audit\"' -> { action: 'update', payload: { id: '123', title: 'Cost Audit' } }"
      ],
      "clarificationPolicy": "Ask one direct question when the chosen action is missing required fields."
    }
  }
}
```

### Why this example is stricter

- it uses `oneOf` to separate action-specific shapes
- it uses `const` to discriminate actions
- it uses `anyOf` to require at least one mutable field on update
- it reduces ambiguity at the schema level instead of relying as much on runtime logic

> **Good next step:** start simple, then move to stricter schemas when the tool becomes more critical or side effects become more expensive.

---

## Part 16 — What to Notice in the Reference Tool

| Location | Why It Matters |
|---|---|
| `name` | Stable machine identifier |
| `title` | Better presentation in clients |
| `description` | First routing contract for models |
| `inputSchema` | Defines both validation and input expectations |
| `oneOf` | Distinguishes action-specific shapes |
| `const` | Makes each action branch explicit |
| `additionalProperties: false` | Prevents loose or invented fields where strictness is desirable |
| `outputSchema` | Makes structured results easier to reason about |
| `annotations` | Native MCP behavior hints |
| `_meta` | Safe place for implementation-specific extensions |

---

## Part 17 — The Forge QA Checklist

Run this checklist before shipping any MCP tool.

### Schema validity

- [ ] Uses `inputSchema`, not `input_schema`
- [ ] Uses `outputSchema`, not `output_schema`
- [ ] Includes only valid MCP Tool fields unless extensions are intentionally placed in `_meta`
- [ ] Does not present `version`, `type`, `auth`, `metadata`, or embedded prompt blocks as standard MCP Tool fields
- [ ] Root input schema is an object
- [ ] Root output schema is an object if `outputSchema` is provided

### Tool guidance quality

- [ ] `description` explains when to call the tool
- [ ] `description` includes at least one “do not use for…” rule
- [ ] input properties have meaningful `description` text
- [ ] enums or discriminators are used where action sets are closed
- [ ] string lengths are bounded where useful
- [ ] outputs are predictable and documented

### Behavioral hints

- [ ] `annotations.readOnlyHint` is set correctly
- [ ] `annotations.destructiveHint` reflects real side effects
- [ ] `annotations.idempotentHint` reflects retry safety
- [ ] `annotations.openWorldHint` reflects external-world interaction
- [ ] open-world tool outputs are treated as potentially untrusted content

### Authoring and orchestration quality

- [ ] example user → tool mappings exist somewhere in docs or `_meta`
- [ ] ambiguous requests are handled with at most one clarifying question
- [ ] missing required fields trigger a concise clarification instead of a blind tool call
- [ ] out-of-scope requests are declined instead of force-fit into the tool

### Schema strictness

- [ ] `additionalProperties: false` is used where unexpected keys would be harmful
- [ ] `oneOf` / `anyOf` / `const` are used where action-specific shapes improve correctness
- [ ] strictness level matches the tool’s operational risk

---

## Part 18 — Five Laws of MCP Tool Craftsmanship

**1. Always say when not to use the tool.**  
Negative guidance reduces false positives.

**2. Describe every important input field.**  
Bare schemas validate; described schemas guide.

**3. Use `annotations` intentionally.**  
These are native MCP behavior hints — use them.

**4. Keep schema and orchestration separate.**  
Put MCP fields in the Tool object. Put normalization, clarification, and internal routing logic in docs, `_meta`, or runtime logic.

**5. Prefer explicit structure over implied behavior.**  
Models do better when actions, constraints, and outputs are concrete.

---

## Part 19 — Recommended Heuristics That Are Not Spec Requirements

Some patterns are very effective, but they are **recommendations**, not MCP requirements.

Examples:

- including a few concrete user → tool examples
- using CRUD verbs for common persistence tools
- asking one clarifying question when a required field is missing
- preferring concise, outcome-oriented titles
- adding a negative rule to the description

These are often high-value conventions, but they should be presented as **recommended practices**, not as mandatory parts of the MCP spec.

> **Good standard:** be strict about the schema, and pragmatic about the heuristics.

---

> *Forging Intelligent Systems with Purpose* — **VibeForge Systems**
