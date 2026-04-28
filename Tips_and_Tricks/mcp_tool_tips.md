# MCP TOOL TIPS — VibeForge One‑Pager

**Purpose**
Short, authoritative guidance for embedding LLM‑visible context into MCP tools.

---

## Required Fields
- **name** — Unique id (3–64 chars).
- **version** — Semantic version (e.g., `1.0.0`).
- **type** — `"tool"`.
- **description** — High‑level intent mapping and when NOT to call the tool.
- **input_schema** — JSON Schema with parameter descriptions.
- **output_schema** — JSON Schema for responses.

---

## Where to Embed LLM Guidance (Four Places)
1. **description** — High‑level intent mapping, negative rules, 2–3 example mappings.
2. **input_schema.properties.*.description** — Parameter‑level examples, formats, maxLength hints.
3. **prompts** — LLM‑facing blocks: `normalize`, `validate`, `infer`. Include explicit call/no‑call rules.
4. **metadata** — Tags, short usage hints, and extra examples.

---

## Recommended Prompt Blocks
- **normalize** — Map user text → structured action. Include 3 example mappings.
- **validate** — Enforce required fields per action; return clarifying question if missing.
- **infer** — Disambiguate intent using context; ask only when confidence is low.

---

## 5 Best‑Practice Rules
1. **State when not to call the tool.** Example: "Do NOT call for brainstorming."
2. **Embed 3 concrete examples.** Use short, outcome‑anchored phrases.
3. **Keep parameter descriptions prescriptive.** Include `minLength`/`maxLength` where relevant.
4. **Prefer CRUD verbs for actions.** Use `create|get|update|delete`.
5. **Ask one clarifying question when required fields are missing.** Keep it direct.

---

## 3 Short Examples
- **Save**: "Save this as a new prompt" → `create` with `{ title, body }`.
- **Fetch**: "Get my last saved prompt" → `get`.
- **Change**: "Rename prompt 123 to 'Cost Audit'" → `update` with `{ id: '123', title: 'Cost Audit' }`.

```
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


---

## Validation Checklist
- [ ] `description` includes when to call and when not to call.
- [ ] `input_schema` fields have `description` and `maxLength` where needed.
- [ ] `prompts.normalize` contains 3 example mappings.
- [ ] `prompts.validate` lists required fields per action.
- [ ] `metadata.examples` includes at least 3 user→tool mappings.

---

**Signature line**
*Forging Intelligent Systems with Purpose* — VibeForge Systems
