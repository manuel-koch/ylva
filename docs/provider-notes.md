# Provider notes

What each supported model provider does and does not report, as it affects
Ylva's behavior. For configuration see [configuration](./configuration.md);
for the implemented features see [features](./features.md).

## Usage and cost reporting

Ylva totals only costs that the provider itself reports in a response (see
[model usage costs](./features.md#model-usage-costs)). It never computes a cost
from a price table.

| Provider | Tokens in final stream frame | Cost reported per request | Counted in the cost total |
| --- | --- | --- | --- |
| OpenRouter | yes | yes: `usage.cost`, in USD | yes |
| GitHub Copilot | yes | yes: `usage.copilot_usage.total_nano_aiu`, in AI credits (`AIC`) | yes |
| Ollama (local) | yes | no | no |
| Ollama Cloud | yes | no | no |
| Plain OpenAI-compatible | yes | no (not part of the OpenAI Chat Completions protocol) | no |

### OpenRouter

- The cost is part of the usage-only SSE frame and is read by the provider's
  own parser; see
  [model usage cost ledger](./implementation-notes.md#model-usage-cost-ledger).
- The currency is always USD.

### GitHub Copilot

- Chat and model listing use the OpenAI-compatible API at the base URL the
  token exchange returns (fallback `https://api.githubcopilot.com`). Ylva
  reads the per-request cost from the final usage frame:
  `usage.copilot_usage.total_nano_aiu` divided by 1e9 gives GitHub AI Credits,
  totalled under the currency `AIC`, GitHub's own billing unit (no USD
  conversion; see [GitHub's billing docs][copilot-billing]). Copilot bills in AI
  credits since June 2026; premium-request multipliers apply only to legacy
  annual Pro/Pro+ plans and are not tracked. The field comes from GitHub's own
  clients' source; it has not been confirmed against a live account, and a
  frame without it simply records no cost.
- Authentication is the device flow with a long-lived GitHub OAuth token
  (`ghu_…`) that has no refresh token and is revoked after a year of
  inactivity. It is exchanged for a session token (about 25–30 minutes) held
  in memory and refreshed five minutes before expiry. Personal access tokens
  cannot be exchanged and are rejected.
- Risk: the API is undocumented and uses the *VS Code Copilot Chat* client id and
  editor headers, which Ylva hardcodes. It can change without notice, and using
  another application's identity may conflict with GitHub's terms; excessive
  request rates can get Copilot access suspended. Ylva therefore spaces
  outbound Copilot requests (chat and model listing, per connection) at least
  2 s apart; the interval is fixed and not configurable. HTTP retries inside
  one chat request are not spaced.
- `X-Initiator` is undocumented. Ylva sends `user` when the last message is
  one the user wrote and `agent` for tool-result and other system-driven
  continuations, the rule VS Code Copilot Chat, Zed and LiteLLM follow. Its
  effect on billing is unverified. Compaction summary
  requests end with an implicit user message and are sent as `agent`.
- Each `/models` entry lists its `supported_endpoints`. Models that list
  `/responses` but not `/chat/completions` (the `gpt-5.x`/`gpt-6.x`, `grok`
  and `codex` families) fail on `/chat/completions` with HTTP 400
  `unsupported_api_for_model`, so Ylva sends them to the Responses API
  (`/responses`, `store: false`, `reasoning.effort` plus a reasoning summary)
  and every other model to `/chat/completions`. Reasoning items are not
  carried between requests. `/models` hides non-chat `capabilities.type`
  entries and models serving neither endpoint. Selectable reasoning efforts come from each model's
  `capabilities.supports.reasoning_effort` list (absent for models without
  reasoning), the input size from `capabilities.limits.max_prompt_tokens`
  (else `max_context_window_tokens`) and image or PDF input from
  `capabilities.supports.vision` plus `limits.vision.supported_media_types`.
  Streaming, tool calls, the reasoning request field and the Responses API
  event shapes and `store: false` have not been smoke-tested against the live
  service by the implementer; a captured Responses stream carries the cost as
  a top-level `copilot_usage` on the `response.completed` event.

### Ollama and Ollama Cloud

- Chat responses carry token counts only (`prompt_tokens`, `completion_tokens`,
  `prompt_tokens_details.cached_tokens`) and, on the native API, durations. No
  request option enables a cost field.
- Ollama Cloud prices usage in USD per model and token class (input, cached
  input, output per million tokens; some models have off-peak rates). The
  rates are published as an HTML table on <https://ollama.com/pricing>, not as
  an API.
- Cost is available only at account level: `GET /api/usage` (usage in USD,
  optionally per model; recent usage may lag) and `GET /api/balance`
  (remaining credits; 10 requests per minute).
- Decision: Ylva does not derive a cost for Ollama. A price table would be
  brittle (rates and off-peak windows change), and the account-level APIs
  include usage from other clients, so neither is a provider-reported
  per-request cost.

[copilot-billing]: https://docs.github.com/en/copilot/concepts/billing-and-usage/individuals/billing
