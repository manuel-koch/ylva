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
| Ollama (local) | yes | no | no |
| Ollama Cloud | yes | no | no |
| Plain OpenAI-compatible | yes | no (not part of the OpenAI Chat Completions protocol) | no |

### OpenRouter

- The cost is part of the usage-only SSE frame and is read by the provider's
  own parser; see
  [model usage cost ledger](./implementation-notes.md#model-usage-cost-ledger).
- The currency is always USD.

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
