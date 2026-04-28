# ⚒️ MCP Tool Tips — The VibeForge Developer's Field Guide

> **TL;DR** — An LLM cannot guess your tool's intent from its name alone. Every field in your tool definition is a signal. The more precise your signals, the smarter your tool behaves. This guide shows you exactly where to put those signals and why each one matters.

---

## ⚠️ What Happens Without This

Picture this: you ship a tool called `process-item`. No description. Parameters named `data` and `opts`. No examples. No negative rules.

The model stares at it like a vending machine with no labels. It starts guessing. Sometimes it guesses right. More often it calls your tool when the user was just brainstorming, or skips it entirely when the user needed it most. In worst-case scenarios it gets stuck in a loop — calling your tool, getting confused by the output, calling it again.

**Ambiguous tools don't just perform poorly — they erode trust in your entire system.**

The fixes are not complicated. They are specific. This guide walks you through all of them.

---

## Part 1 — The Anatomy of a Well-Forged Tool

These six fields are the skeleton of every MCP tool. Skipping any of them is like shipping a car without dashboard gauges — it may run, but nobody knows what's happening inside.

| Field | What It Does | Common Mistake |
|---|---|---|
| `name` | Unique identifier, 3–64 chars | Too generic (`"tool1"`) or too verbose (`"vibeforge-artifact-management-crud-interface-v2"`) |
| `version` | Semantic version (`1.0.0`) | Omitting it entirely — breaks reproducibility when tools evolve |
| `type` | Always `"tool"` | Forgetting this field; some runtimes reject the definition silently |
| `description` | High-level intent + when NOT to call | Writing only what the tool does, never what it *doesn't* do |
| `input_schema` | JSON Schema for every parameter | Fields with no `description` — the model has no guidance on what to send |
| `output_schema` | JSON Schema for responses | Skipping this entirely — the model can't reason about what it gets back |

> **Pro tip:** Your `description` is the single most leveraged field in the entire definition. The model reads it first, reasons against it, and decides whether to invoke the tool at all. Treat it like a routing contract, not a tooltip.

---

## Part 2 — Four Levers of LLM Precision

Think of your tool definition as four concentric layers. Each layer adds a finer level of guidance. Miss an outer layer and the inner ones lose half their power.

### 🔵 Layer 1 — `description` (The Front Door)

This is the first thing the model sees. It maps high-level user intent to your tool. Done well, it answers three questions at once:

- What does this tool accomplish?
- What user phrases or intents should trigger it?
- What should **never** trigger it?

**Without negative rules**, the model treats your tool as a candidate for anything vaguely related. Add two or three explicit "Do NOT call for..." statements and watch false-positive invocations drop dramatically.

```
// ❌ Weak
"description": "Handles prompts."

// ✅ Strong
"description": "CRUD interface for saved prompt artifacts. Call when the user
wants to persist, retrieve, rename, or delete a stored item. Do NOT call for
brainstorming, drafting, or general advice. Example mappings: 'Save this prompt'
-> create; 'Get my last prompt' -> get; 'Rename prompt 123' -> update."
```

---

### 🟢 Layer 2 — `input_schema.properties.*.description` (The Parameter Guide)

Every parameter your tool accepts is an opportunity to teach the model what good input looks like. Don't just name the field — describe it with examples, formats, and constraints.

```
// ❌ Weak
"title": { "type": "string" }

// ✅ Strong
"title": {
  "type": "string",
  "minLength": 3,
  "maxLength": 256,
  "description": "Short, outcome-oriented title. Example: 'Glass Receipt: cost per agent'."
}
```

The `minLength`/`maxLength` hints aren't just validation — they calibrate the model's expectations for what "a title" looks like in your domain.

---

### 🟡 Layer 3 — `prompts` Blocks (The Reasoning Engine)

The `prompts` section is where you teach the model *how to think* about your tool, not just when to call it. Three blocks cover the full reasoning funnel:

- **`normalize`** — Translate loose user language into a structured action. The model lands here first.
- **`validate`** — Enforce that the required fields for the chosen action are actually present. If they're missing, return a clarifying question instead of calling the tool with bad data.
- **`infer`** — When intent is genuinely ambiguous, use context to pick the most likely action. Ask one question if confidence is still low.

Think of these three as: *What did they mean? → Do I have what I need? → If still unsure, ask once.*

---

### 🔴 Layer 4 — `metadata` (The Discovery Layer)

Tags, usage hints, and extra examples in `metadata` act as a secondary index. They help the model surface your tool in broader retrieval contexts and give it additional worked examples to pattern-match against. Don't underestimate this layer — it's often what tips a borderline invocation decision in the right direction.

---

## Part 3 — Teaching the Model to Think: The Prompt Blocks in Detail

| Block | Job | What Breaks Without It |
|---|---|---|
| `normalize` | Maps natural language → `{ action, payload }` | Model invents its own action names or passes raw user text as the payload |
| `validate` | Guards against missing required fields | Tool receives incomplete data, returns errors, model retries blindly |
| `infer` | Resolves ambiguity using context | Model picks the wrong action on a coin flip, or asks five questions instead of one |

**The normalize block needs at least 3 example mappings.** Not because 3 is magic — because concrete examples anchor the model's pattern recognition. Abstractions alone drift. Examples stick.

**The validate block should return a question, not an error.** When required fields are missing, the right response is *asking for them*, not failing silently. Keep the question short and direct.

**The infer block should have a confidence threshold.** Below `0.7`, ask one question. Above it, commit. A model that asks five clarifying questions feels broken. A model that commits confidently on reasonable context feels intelligent.

---

## Part 4 — Five Laws of Tool Craftsmanship

These aren't preferences. They're the difference between a tool that works and a tool that works *reliably*.

**1. Always state when NOT to call the tool.**

The model considers every available tool for every user message. Negative rules are circuit breakers. Without them, your tool leaks into conversations it was never meant for.

> *"Do NOT call for brainstorming, speculative drafting, or when the user asks for general advice."*

**2. Embed exactly 3 concrete examples in description and normalize.**

Three examples establish a pattern. One is a fluke. Two is a coincidence. Three is a rule the model can generalize from. Make them short, outcome-anchored, and cover different action types.

**3. Keep parameter descriptions prescriptive, not descriptive.**

"A string for the title" tells the model nothing useful. "A short, outcome-oriented title, 3–256 characters. Example: 'Glass Receipt: cost per agent'" tells the model exactly what to produce.

**4. Use CRUD verbs for action enums: `create | get | update | delete`.**

These four verbs map cleanly to the model's training on databases, APIs, and file systems. Custom verb names like `persist`, `fetch`, `modify`, or `remove` introduce unnecessary translation overhead. Stick to the standard four.

**5. Ask one clarifying question when required fields are missing.**

One. Not a list. Not a form. One direct question targeting the most critical missing field. The user answers it, you have what you need, the interaction moves forward. Multi-question clarification flows feel like bureaucracy. Keep it surgical.

---

## Part 5 — Three Patterns Every Tool Should Know

These three patterns cover the vast majority of real-world user intent. Learn them, annotate your tool with them, and you'll handle 80% of interactions without ambiguity.

---

### Pattern A — The Save Intent

> **User says:** *"Save this as a new prompt"*
> **Maps to:** `create` with `{ title, body }`

**Why this mapping works:** The verb "save" combined with "new" signals unambiguous creation intent. No existing ID is referenced. The payload is whatever the user just produced. This is the cleanest pattern — a single-pass normalize with no inference needed.

---

### Pattern B — The Fetch Intent

> **User says:** *"Get my last saved prompt"*
> **Maps to:** `get`

**Why this mapping works:** "Get" + "last" signals retrieval without modification. The recency qualifier ("last") gives the tool enough context to identify the target without an explicit ID. Your infer block can resolve "last" against `recent_actions` in `requester_context`.

---

### Pattern C — The Change Intent

> **User says:** *"Rename prompt 123 to 'Cost Audit'"*
> **Maps to:** `update` with `{ id: '123', title: 'Cost Audit' }`

**Why this mapping works:** An explicit ID (`123`) and a change verb (`rename`) are present. The normalize block should always check for existing IDs first — their presence is the strongest signal that the user intends an update, not a create. Never create a duplicate when an ID is in the message.

---

## Part 6 — The Full Reference Schema

The schema below is a complete, production-ready MCP tool definition implementing every principle in this guide. Read it alongside the annotations table that follows.

```json
{
  "name": "vibeforge-crud-tool",
  "version": "1.0.0",
  "type": "tool",
  "description": "Architecture-led CRUD interface for persistent artifacts. Use this tool when the user expresses a clear intent to create, fetch, update, or delete a stored item. Do NOT call for brainstorming, speculative drafting, or when the user asks for general advice. Example mappings: 'Save this as a new prompt' -> create; 'Get my last prompt' -> get; 'Rename prompt 123' -> update.",
  "auth": {
    "type": "external",
    "fields": {
      "credential_id": {
        "type": "string",
        "required": true,
        "description": "Reference to a stored credential. Example: 'cred_abc123'."
      }
    }
  },
  "input_schema": {
    "type": "object",
    "properties": {
      "action": {
        "type": "string",
        "enum": ["create", "get", "update", "delete"],
        "description": "Operation to perform. Use 'create' to persist new artifacts; 'get' to retrieve; 'update' to modify; 'delete' to remove."
      },
      "payload": {
        "type": "object",
        "description": "Structured data for the action. For create: { title, body, tags }. For update: { id, title?, body?, tags? }. For get/delete: { id }.",
        "properties": {
          "id": {
            "type": "string",
            "description": "Unique identifier for the artifact. Required for get, update, delete."
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
            "description": "Full artifact text. Keep focused: state the outcome, the constraints, and the expected result."
          },
          "tags": {
            "type": "array",
            "items": { "type": "string" },
            "description": "Categorical tags; use for discovery and policy routing."
          }
        }
      },
      "requester_context": {
        "type": "object",
        "description": "Optional context to help inference: { user_id, role, recent_actions }."
      }
    },
    "required": ["action"]
  },
  "output_schema": {
    "type": "object",
    "properties": {
      "status": {
        "type": "string",
        "description": "Result status. Example: 'success', 'error'."
      },
      "data": {
        "type": "object",
        "description": "Returned payload for get/create/update operations."
      },
      "errors": {
        "type": "array",
        "items": { "type": "string" },
        "description": "List of error messages, if any."
      }
    },
    "required": ["status"]
  },
  "prompts": {
    "normalize": "Map user language to a structured action. Rules: 1) If user asks to persist or save content -> action='create'. 2) If user asks to retrieve a named or recent item -> action='get'. 3) If user asks to change an existing item -> action='update'. 4) If user asks to remove an item -> action='delete'. Examples: 'Save this as a new prompt' -> { action: 'create', payload: { title: '...', body: '...' } }; 'Get my last saved prompt' -> { action: 'get' }; 'Rename prompt 123 to \"Cost Audit\"' -> { action: 'update', payload: { id: '123', title: 'Cost Audit' } }. Do NOT call this tool for brainstorming, drafting, or when the user asks for general advice.",
    "validate": "Confirm required fields exist for the chosen action. If required fields are missing, return a short clarifying question instead of calling the tool. Validation rules: create -> payload.title and payload.body required; get/delete -> payload.id required; update -> payload.id and at least one of title/body/tags required. Keep clarifying questions short and direct.",
    "infer": "When user intent is ambiguous, infer the most likely action using requester_context and recent_actions. If confidence < 0.7, ask one clarifying question. Preference order: update (if an id or 'change' verb present), create (if user provides content), get (if user asks for 'show' or 'fetch'), delete (if user uses 'remove' or 'delete')."
  },
  "metadata": {
    "category": "mcp-tool",
    "tags": ["Glass Box AI", "Architecture-led delivery", "CRUD", "vibeforge"],
    "llm_usage": "Call only for explicit CRUD intent. Outcome focus: executive clarity, governance posture, and investment sequencing.",
    "examples": [
      "User: 'Save this as a new prompt' -> { action: 'create', payload: { title: '...', body: '...' } }",
      "User: 'Get my last prompt' -> { action: 'get' }",
      "User: 'Rename prompt 123 to \"Cost Audit\"' -> { action: 'update', payload: { id: '123', title: 'Cost Audit' } }"
    ]
  }
}
```

### What to Notice in This Schema

| Location | What It Does | Why It's There |
|---|---|---|
| `description` | 3 example mappings + 1 negative rule | Routing contract — the model's first decision gate |
| `action.enum` | Standard CRUD verbs only | Eliminates translation overhead from custom verb names |
| `action.description` | Per-verb usage guidance | Prevents the model from picking `delete` when `update` was intended |
| `payload.description` | Shape of payload per action type | Teaches the model what to construct *before* it fills parameters |
| `title.minLength/maxLength` | Bounds on string length | Calibrates model output to your domain's expectations |
| `body.description` | Outcome-focused writing guidance | Produces better artifact content, not just valid schema |
| `requester_context` | Optional inference fuel | Gives the infer block real data to work with |
| `prompts.normalize` | 4 rules + 3 examples + 1 negative rule | Full routing specification in a single block |
| `prompts.validate` | Per-action required field rules | Prevents bad data from reaching your backend |
| `prompts.infer` | Confidence threshold + preference order | Deterministic disambiguation without over-asking |
| `metadata.examples` | 3 additional user→tool mappings | Reinforces normalize patterns in the discovery layer |

---

## Part 7 — Before You Ship: The Forge QA Checklist

Run through this before every tool release. Every unchecked box is a potential bad invocation in production.

- [ ] `description` includes **when to call** and at least one **when NOT to call** rule
- [ ] `input_schema` fields all have `description` — no bare `"type": "string"` parameters
- [ ] `input_schema` string fields have `maxLength` where unbounded input would be a problem
- [ ] `prompts.normalize` contains **3 example user→action mappings**
- [ ] `prompts.validate` lists **required fields per action** with a clarifying question fallback
- [ ] `prompts.infer` has a **confidence threshold** and a **preference order** for actions
- [ ] `metadata.examples` includes **at least 3 user→tool mappings**
- [ ] Tool has been tested with **ambiguous input** — does it ask one question or hallucinate?
- [ ] Tool has been tested with **missing required fields** — does it ask or fail silently?
- [ ] Tool has been tested with **out-of-scope input** — does it decline gracefully?

---

> *Forging Intelligent Systems with Purpose* — **VibeForge Systems**
