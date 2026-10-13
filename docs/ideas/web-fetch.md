# Web Fetch

Add more backends for tool `web_fetch`:

- CLI tools like _camofox browser_ ?

- CLI tools like _lightpanda_ ?

- Alternative headless browser implementations ?
  Ideally one that is less detected as a bot to reduce being banned or rejected
  on web servers.

- We could allow LLM to pick the backend via an `web_fetch` argument
  ( to maybe try multiple backends when one fails or doesn't return enough/good
  data ). Checking at ylva startup which backends are available.
