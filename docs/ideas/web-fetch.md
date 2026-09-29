# Web Fetch

Add new tool `web_fetch` to allow LLM to fetch arbitrary www content via `GET`
request on a given `url`.

Initial implementation will just do the `GET` request.
We may later introduce functionality to decide which _fetch_ backend implementation
to use, e.g. use external service instead of local `GET`.

Local implementation:

- Use Ylva user-agent for `GET` requests
- Convert fetched output ( likely html/text/json ) to markdown ?
- Use CLI tools like _camofox browser_ ?
  - Other headless browser implemenations ? Ideally one that is less detected as a
    bot to reduce being banned or rejected on web servers.
  - Alternatives ?
