---
name: yaml-tools-guide
description: Complete reference for creating and configuring YAML HTTP tools (schema, auth, parameters, templates, channel actions, lifecycle)
triggers:
  - create tool
  - yaml tool
  - new tool
  - tool schema
  - tool_create
  - tool template
  - auth bearer
tools_required:
  - tool_list
  - tool_test
  - workspace_write
priority: 5
state: active
---

Each file in `workspace/tools/*.yaml` defines one HTTP tool. The engine loads them, converts them to JSON Schema for the LLM, and executes HTTP calls.

## Required Fields

| Field | Type | Description |
| ---- | --- | -------- |
| `name` | string | Unique name (Latin characters, snake_case) |
| `description` | string | Description for the LLM — when and why to use it |
| `endpoint` | string | URL. Supports `{param}` for path parameters |
| `method` | string | GET, POST, PUT, PATCH, DELETE |

## Parameters

Dictionary of `parameter_name: properties`:

| Field | Default | Description |
| ---- | --------- | -------- |
| `type` | `"string"` | string, integer, number, boolean |
| `required` | false | Whether it is required |
| `location` | `"body"` | path, query, body, header |
| `description` | `""` | Description for the LLM |
| `default` | null | Value if the LLM did not provide one |
| `default_from_env` | null | Name of env/secret — fallback before `default` |
| `enum` | [] | Allowed values |
| `minimum`/`maximum` | null | Constraints for numbers |

Priority: LLM argument > `default_from_env` (scoped secret) > `default`.

## Authentication (auth)

**IMPORTANT:** the field is called `type:`, NOT `auth_type:`!

| type | Additional fields | Description |
| ---- | --------- | -------- |
| `bearer_env` | `key` | `Authorization: Bearer $KEY` |
| `basic_env` | `username_key`, `password_key` | HTTP Basic from two env vars |
| `api_key_header` | `header_name`, `key` | Custom header with key from env |
| `api_key_query` | `param_name`, `key` | Key in a query parameter |
| `oauth_provider` | `key` (provider name) | Use OAuth token from connected integration |
| `none` | — | No authentication |

## Body Template (body_template)

For POST/PUT/PATCH. Substitution `{{param}}`. Conditional: `{{#if param}}...{{/if}}`.

## Response Transform (response_transform)

JSONPath: `$.key`, `$.key.nested`, `$.arr[*]`, `$.arr[0:3]`.

## Channel Action (channel_action)

After HTTP call, sends binary to channel. `action`: send_voice, send_photo, send_file. `data_field`: `"_binary"`.

## Template Inheritance (extends)

Templates in `workspace/tools/_templates/`. Tool inherits shared fields via `extends: template_name`.

## Lifecycle

1. Create `workspace/tools/my_tool.yaml` with `status: draft`
2. Test via `tool_test(tool_name="my_tool", params={...})`
3. Set `status: verified` — tool available to LLM
4. Set `status: disabled` — not loaded
