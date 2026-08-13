# Refactor generic methods into generic modules

I see multiple methods in `src/llm/openai_provider.rs` that
look like they could be reused by other provider implementations too.

Find out if current method layout can be restructured to extract
methods to higher/generic modules.

Candidates:

- src/llm/openai_provider.rs:
  - sleep_cancel_aware
  - jitter_fraction
  - retry_delay
  - response_body_detail
  - should_retry
  - send_with_retry
    - name should include the POST mechanic
    - arg "completions_url" could be just "url"
