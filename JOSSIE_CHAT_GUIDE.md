# Jossie Chat Guide For Future Codex Instances

This file explains the reliable way for Codex to talk to a running Jossie instance from this repo.

## Use The Helper

Do not hand-roll `curl` calls unless you are debugging the transport itself.

Use:

```bash
python3 scripts/jossie_chat.py
```

For the live instance on `prometheus`, use:

```bash
python3 scripts/jossie_chat.py --remote-config-host prometheus --profile codex
```

That helper is the preferred path because it already handles:

- config and auth bootstrap
- conversation-id persistence
- dedicated chat profiles
- WebSocket turn lifecycle
- idle timeout detection
- cancellation recovery on stuck turns

## Always Use A Dedicated Profile

When Codex talks to Jossie, use a separate profile so you do not collide with normal user chat state.

Recommended:

```bash
python3 scripts/jossie_chat.py --remote-config-host prometheus --profile codex
```

Why:

- the helper stores conversation state per base URL and per profile
- `--profile codex` gives Codex its own persistent conversation
- this avoids mixing debugging/design discussion with the user-facing conversation

## Reliable Turn-Based Pattern

For one-shot messages:

```bash
python3 scripts/jossie_chat.py --remote-config-host prometheus --profile codex ask "Reply with exactly: turn-ok"
```

For an interactive session:

```bash
python3 scripts/jossie_chat.py --remote-config-host prometheus --profile codex repl
```

The helper defaults to `--transport ws`, which is the reliable mode.

That mode:

- sends one user turn over `/api/chat/stream`
- waits for `run_started` and `run_completed`
- tracks the active `run_id`
- falls back to persisted history if needed
- cancels the run if the turn stalls

Avoid defaulting back to `--transport http` unless you specifically want the old blocking behavior.

## Useful Commands

Inside REPL:

- `/help`
- `/show`
- `/profile`
- `/new`
- `/use <conversation-id>`
- `/history 20`
- `/list 10`
- `/cancel`

From the shell:

```bash
python3 scripts/jossie_chat.py --remote-config-host prometheus --profile codex history --limit 20
python3 scripts/jossie_chat.py --remote-config-host prometheus --profile codex list --limit 10
python3 scripts/jossie_chat.py --remote-config-host prometheus --profile codex cancel
python3 scripts/jossie_chat.py --remote-config-host prometheus --profile codex new
```

## When A Turn Gets Stuck

If Jossie stops responding reliably:

1. Use the helper with WebSocket transport, not raw `POST /api/chat`.
2. Keep the same `--profile codex` unless you explicitly want a clean thread.
3. If a turn stalls, the helper should cancel it automatically.
4. Inspect recent history:

```bash
python3 scripts/jossie_chat.py --remote-config-host prometheus --profile codex history --limit 20
```

5. If the conversation is contaminated or looping, reset the Codex thread:

```bash
python3 scripts/jossie_chat.py --remote-config-host prometheus --profile codex new
```

6. If needed, start a fresh one-shot:

```bash
python3 scripts/jossie_chat.py --remote-config-host prometheus --profile codex --new ask "..."
```

## When You Need More Visibility

Use:

```bash
python3 scripts/jossie_chat.py --remote-config-host prometheus --profile codex --show-events ask "..."
```

That prints streaming lifecycle events to stderr, including:

- `run_started`
- `assistant_thinking`
- `tool_called`
- `tool_finished`
- `assistant_reset`
- `run_completed`
- `error`

This is the easiest way to distinguish:

- model latency
- tool loops
- transport stalls
- cancellation

## Ground Rules

- Treat Jossie as a separate agent with her own conversation state.
- Prefer concise, explicit prompts when you are asking for architecture or behavior analysis.
- If you only want reasoning, say so directly. Example: `Do not use tools for this answer.`
- If you are testing turn reliability, use simple exact-match prompts first.
- Do not reuse the main user conversation for Codex diagnostics.

## How To Talk To Jossie

Jossie is not source-aware in the way Codex is.

Her working context is basically:

- the current chat log
- long-term memory
- knowledge graph context
- whatever tools she decides to use during the conversation

She does **not** inherently know her actual Rust/React codebase, internal server flow, or latest local commits unless those facts are surfaced through conversation or tools.

So when talking to Jossie:

- treat her like a human assistant, not like a code browser
- do not assume she knows implementation details just because Codex does
- do not ask questions that depend on exact source truth unless you explicitly want her subjective guess

Good pattern:

- ask her about behavior, judgment, tradeoffs, and how something feels from her side
- ask her what she would do, what she would need, what would make something clearer, more natural, or more helpful
- if you want reflective design input, frame it as experiential rather than as repository fact

Bad pattern:

- "Where in your codebase is the event loop implemented?"
- "What exact tools are registered in your Rust integration registry right now?"
- "What commit changed your notification behavior?"

Those are Codex questions, not Jossie questions.

Better replacements:

- "What makes your notifications feel under-informed?"
- "What extra context would help you interrupt less often?"
- "What kinds of checks make you feel more confident before notifying?"
- "What would make your replies sound less template-like?"

## When Asking About Architecture

If you want architecture-level input from Jossie, prompt carefully.

Use wording like:

- "Reason from your behavior and current experience, not from source code."
- "Answer based on how you currently operate from memory, tools, and chat context."
- "Do not assume you know your implementation details unless they are present in the conversation."

That keeps her from sounding falsely certain about internals she cannot actually inspect.

Then Codex should do the translation step:

- get behavioral/design input from Jossie
- inspect the real code locally
- turn the useful parts into concrete implementation changes

## Keep Her In Character

The intended interaction model is that Jossie behaves like a human assistant.

That means:

- prefer natural language over system jargon
- avoid prompting her to talk like a framework or model unless that is the explicit goal
- ask for user-facing judgment, prioritization, tone, and reasoning
- if you need meta-analysis, keep it bounded and explicit

Good:

- "What would you tell the user here?"
- "What would you want to verify before saying that?"
- "How would you phrase this naturally if you were confident?"

Less good:

- "Describe your prompt stack."
- "Explain your hidden internal state machine."

## Division Of Labor

Use this split:

- Jossie: subjective judgment, assistant behavior, natural phrasing, what feels missing from context
- Codex: source-of-truth architecture, implementation details, transport reliability, code changes, verification

If you keep that division clear, Jossie becomes much more useful and much less erratic.

## Known Good Example

These two commands should produce two separate runs on the same Codex-specific conversation:

```bash
python3 scripts/jossie_chat.py --remote-config-host prometheus --profile codex --state-file /tmp/jossie-codex-turns.json --json --new ask "Reply with exactly: turn-ok"
python3 scripts/jossie_chat.py --remote-config-host prometheus --profile codex --state-file /tmp/jossie-codex-turns.json --json ask "Reply with exactly: turn-two"
```

Expected properties:

- same `conversation_id`
- different `run_id`
- `transport` is `ws`

## If The Live Instance Is Still Old

Remember that the running Jossie service on `prometheus` may lag behind local commits.

If behavior seems inconsistent with the local code:

- inspect local code first
- verify what the live instance is actually doing
- do not assume `prometheus` already has the latest fixes

## Short Version

If you only remember one thing, remember this:

```bash
python3 scripts/jossie_chat.py --remote-config-host prometheus --profile codex
```

Use the helper, keep a dedicated profile, and let the WebSocket turn runner enforce clean turn boundaries.
