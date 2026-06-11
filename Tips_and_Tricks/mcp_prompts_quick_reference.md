# 🧭 MCP Prompts Quick Reference

> **Use this card when designing or reviewing MCP prompts.**
>
> Goal: make prompts easy for clients to surface, easy for users to understand, and easy for models to fill with the right arguments.

---

## 1. MCP Prompt Fields at a Glance

| Field | Required? | What It Does |
|---|---|---|
| `name` | Yes | Stable prompt identifier |
| `title` | No | Human-readable display name |
| `description` | No | Explains what the prompt is for |
| `arguments` | No | Declares structured prompt inputs |
| `_meta` | No | Implementation-specific extension space |

> **Remember:** prompts are **templates surfaced by the server**, not hidden reasoning blocks embedded in a tool schema.

---

## 2. Start With These Three Questions

Before writing the prompt definition, answer:

1. **What task is this prompt helping the user perform?**
2. **What inputs does the user need to provide?**
3. **What should the prompt make easier, faster, or more consistent?**

If the prompt’s purpose is fuzzy, users and clients will struggle to choose it.

---

## 3. Description Checklist

A strong `description` usually includes:

- the prompt’s job
- the type of outcome it helps produce
- the expected user inputs
- optional guidance on when to use it

### Weak

```json
{ "description": "Helps write things." }
```

### Better

```json
{
  "description": "Generate a concise architecture review using the supplied system name, goals, constraints, and known risks."
}
```

---

## 4. `arguments` Checklist

Use `arguments` when the prompt benefits from structured inputs.

Good prompt arguments usually have:

- a clear `name`
- a meaningful `description`
- a signal for whether the argument is required

### Example

```json
{
  "arguments": [
    {
      "name": "system_name",
      "description": "Short name of the system being reviewed.",
      "required": true
    },
    {
      "name": "constraints",
      "description": "Known delivery, regulatory, or operational constraints.",
      "required": false
    }
  ]
}
```

### Good practice

- use argument names that are clear and specific
- describe the kind of value expected
- keep required arguments to the true minimum
- avoid vague names like `data`, `stuff`, or `input`

---

## 5. Prompt Design Tips

A good MCP prompt usually does one of these well:

- standardizes a recurring task
- reduces ambiguity in a common workflow
- improves consistency of output shape or tone
- helps the user get started faster

### Strong prompt patterns

- summarize a design or document
- generate a review checklist
- draft a migration plan
- analyze tradeoffs with explicit criteria
- transform raw notes into a structured artifact

### Weak prompt patterns

- too broad to be recognizable
- unclear about expected inputs
- duplicates general chat capability without adding structure
- mixes multiple unrelated tasks into one template

---

## 6. Simple Starter Example

```json
{
  "name": "architecture_review",
  "title": "Architecture Review",
  "description": "Generate a concise architecture review using the supplied system name, goals, constraints, and known risks.",
  "arguments": [
    {
      "name": "system_name",
      "description": "Short name of the system being reviewed.",
      "required": true
    },
    {
      "name": "goals",
      "description": "Primary goals or desired outcomes.",
      "required": true
    },
    {
      "name": "constraints",
      "description": "Known delivery, technical, budget, compliance, or operational constraints.",
      "required": false
    },
    {
      "name": "risks",
      "description": "Known risks, weaknesses, or open concerns.",
      "required": false
    }
  ]
}
```

---

## 7. Common Mistakes

Avoid these:

- making the prompt so generic that it adds no real value
- omitting argument descriptions
- requiring too many fields up front
- using unclear or overloaded argument names
- treating MCP prompts like hidden chain-of-thought instructions instead of reusable prompt templates

---

## 8. Fast QA Checklist

- [ ] Prompt has a clear purpose
- [ ] `description` explains what the prompt helps produce
- [ ] arguments are clearly named
- [ ] arguments describe expected input shape
- [ ] only truly necessary fields are required
- [ ] prompt improves usability over plain chat

---

> **Rule of thumb:** a good MCP prompt should feel like a reusable accelerator for a specific job.
