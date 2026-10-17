# Copilot provider

Add a provider implementation that uses Copilot LLMs. It is not implemented
today; built-in connections currently use Ollama and OpenRouter through the
shared OpenAI-compatible chat transport.

If possible support using GH_TOKEN with content of a personal-access-token.

* Do we need a new slash-command `/connect` to guide user thru the
  process of adding a provider-connection for _copilot_ to current
  profile config ?
  * edit `config.toml` and add a new `[provider_connections.<name>]`
    table
  * update `.env` with appropriate secret envvars ( token we get during
    onboarding process )

## GitHub Copilot API: OAuth Device Flow for Raw HTTP Access (Rust)

### Purpose

How to use the GitHub Copilot Chat API as a raw OpenAI-compatible LLM backend
from a Rust agent harness, with full control over tool calls and access to
**all models available to your account** (including premium/frontier models
like GPT-5.x, Claude Sonnet/Opus, Gemini Pro). This requires the OAuth device
flow with the correct client ID, token exchange, and integration headers — **not**
a PAT, and **not** just a raw OAuth token without exchange.

### Why a PAT Is Not Usable

A fine-grained Personal Access Token (`github_pat_`) **cannot** be used to get a
bearer token for raw HTTP calls to the Copilot API.

* The token exchange endpoint
  `GET https://api.github.com/copilot_internal/v2/token` **rejects PATs** with
  HTTP 403: _"Resource not accessible by personal access token"_.

* A fine-grained PAT with the **Copilot Requests** permission works with the
  Copilot CLI (set via `COPILOT_GITHUB_TOKEN` env var), but the CLI uses a
  **different internal auth code path** that never exposes the Copilot API token
  back to your application.

* The Copilot CLI and SDK are **agent runtimes** — they manage the entire agent
  loop (planning, tool execution, file edits) internally. You cannot intercept
  raw `tool_calls` from the model response. They are unsuitable if your app needs
  to handle tool calls itself.

* Classic PATs (`ghp_`) are not supported at all.

**Only GitHub App user tokens (**`ghu_` **prefix)** obtained through the device
flow with the correct client ID can be exchanged at `copilot_internal/v2/token`
for a Copilot session token usable in raw HTTP requests.

### Two-Tier Model Access: GA vs Premium/Frontier

There are **two tiers** of model access in the Copilot API:

| Auth method | GA models (gpt-4o, etc.) | Premium/preview models (GPT-5.x, Claude Opus, etc.) |
| --- | --- | --- |
| Raw `ghu_` token, no integration headers | ✅ 200 | ❌ 400 `model_not_supported` |
| Session token (`tid=...`) + integration headers | ✅ 200 | ✅ **200** |
| Session token (`tid=...`), no integration headers | ❌ 403 `token not authorized` | ❌ 403 |

**Two requirements** for premium model access:

1. A **Copilot session token** (obtained via token exchange at `copilot_internal/v2/token`)

2. The **integration headers** on every API request (Editor-Version, Editor-Plugin-Version, Copilot-Integration-Id)

Without the exchange, you only get GA models. Without the headers, the session token is rejected entirely.

### The Client ID: Critical for Token Type and Model Access

The client ID used in the device flow determines what kind of token you get,
which determines whether the token exchange works and whether the full model
catalog is accessible.

| Client ID | Type | Token prefix | Token exchange works? | Full model catalog? |
| --- | --- | --- | --- | --- |
| `Iv1.b507a08c87ecfe98` | GitHub App (VS Code Copilot) | `ghu_` | ✅ Yes | ✅ Yes (after exchange) |
| `Ov23li8tweQw6odWQebz` | OAuth App (opencode's own) | `gho_` | ❌ No (404) | ❌ No (GA only) |

**Use** `Iv1.b507a08c87ecfe98` — this is the VS Code Copilot Chat GitHub App
client ID. It produces `ghu_` tokens that are accepted by the
`copilot_internal/v2/token` exchange endpoint and unlock the full model catalog.
This is the same client ID hardcoded in all official Copilot extensions.

### Token Lifetime: ghu_ Is Long-Lived

The `ghu_` OAuth token from the device flow is **long-lived** — it does NOT
expire after 8 hours. The 8-hour expiry applies to GitHub App installation
tokens, not to user-to-server tokens issued via the device flow.

* **No expiry on the** `ghu_` **token by default.** The device flow response
  returns `access_token`, `token_type`, and `scope` — no `expires_in`, no
  `refresh_token`. The token simply does not expire.

* **No refresh token is issued** — none is needed since the access token doesn't expire.

* **The only auto-expiry is 1 year of inactivity.** GitHub says: "GitHub will
  automatically revoke an OAuth token when the token hasn't been used in one
  year." As long as you use the token at least once a year, it persists
  indefinitely.

* **The short-lived Copilot session token** (obtained from
  `copilot_internal/v2/token`) does expire after ~25–30 minutes, but you simply
  re-exchange the long-lived `ghu_` token to get a fresh one — no browser
  interaction needed.

* **Community evidence:** Tokens stored in `~/.config/github-copilot/hosts.json`
  have been reported working for over 2 years, even across subscription lapses.

**You only need the browser-based device flow once.** After that, your app
exchanges the long-lived `ghu_` token for short-lived Copilot session tokens
non-interactively whenever needed.

### Account-Specific Endpoints

The correct API base URL varies by account type. The
`copilot_internal/v2/token` exchange response includes an `endpoints.api` field
that tells you the correct base URL.

| Plan | Endpoint | Notes |
| --- | --- | --- |
| Individual Pro | `api.individual.githubcopilot.com` | Raw `ghu_` token may also work directly [1] |
| Business | `api.business.githubcopilot.com` | Requires exchanged session token |
| Enterprise | Custom enterprise subdomain | Requires exchanged session token; URL from exchange response |

You can also discover the account type and correct endpoint at runtime by
calling `GET https://api.github.com/copilot_internal/user` with the `ghu_`
token:

```rust
#[derive(Deserialize)]
struct CopilotUser {
    copilot_plan: String,          // "individual", "business", "enterprise"
    endpoints: CopilotEndpoints,
}
#[derive(Deserialize)]
struct CopilotEndpoints {
    api: String,                   // e.g. "https://api.individual.githubcopilot.com"
    #[serde(default)]
    proxy: Option<String>,
}
```

### Architecture Overview

```text
┌─────────────────────────────────────────────────────────────┐
│  One-time setup (interactive, ~30 seconds, NEVER repeated    │
│  unless token is revoked)                                    │
│                                                               │
│  1. Request device code from GitHub (client_id: Iv1.b507...)  │
│  2. User visits github.com/login/device, enters code          │
│  3. Poll GitHub → receive ghu_ OAuth token (long-lived)        │
│  4. Save ghu_ token to disk                                    │
└─────────────────────────────────────────────────────────────┘
                        ↓
┌─────────────────────────────────────────────────────────────┐
│  Runtime (non-interactive, your agent loop)                   │
│                                                               │
│  A. Read ghu_ token from disk                                  │
│  B. (Optional) GET copilot_internal/user → discover plan +    │
│     correct endpoints.api for account type                    │
│  C. Exchange ghu_ token → Copilot session token (tid=...)      │
│     GET copilot_internal/v2/token                             │
│     Authorization: token <ghu_>                               │
│     → Returns: session token, expires_at, endpoints.api         │
│     (25-30 min TTL, cache and refresh as needed)              │
│  D. POST {api_base}/chat/completions                           │
│     Authorization: Bearer <session_token>                      │
│     + Integration headers (REQUIRED for session token to work) │
│     with tools, messages → get response                        │
│     Response may contain tool_calls → YOUR app                │
│     executes tools → feed results back → repeat                │
└─────────────────────────────────────────────────────────────┘
```

### Rust Crate Dependencies

```toml
[dependencies]
reqwest = { version = "0.12", features = ["json"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
chrono = { version = "0.4", features = ["serde"] }
dirs = "6"
anyhow = "1"
```

### Step 1: Request a Device Code

```rust
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,     // seconds (usually 900 = 15 min)
    interval: u64,       // minimum seconds between polls (usually 5)
}

const CLIENT_ID: &str = "Iv1.b507a08c87ecfe98"; // GitHub Copilot's GitHub App client ID
// This produces ghu_ tokens that work with the token exchange and unlock the full model catalog.

async fn request_device_code(client: &reqwest::Client) -> anyhow::Result<DeviceCodeResponse> {
    let resp = client
        .post("https://github.com/login/device/code")
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .header("User-Agent", "GitHubCopilotChat/0.26.7")
        .json(&serde_json::json!({
            "client_id": CLIENT_ID,
            "scope": "read:user"
        }))
        .send()
        .await?
        .json()
        .await?;
    Ok(resp)
}
```

Print the `user_code` and `verification_uri` for the user, then poll.

### Step 2: User Authorizes (Manual, ONE TIME ONLY)

Display instructions to the user:

```text
Open this URL in your browser:  {verification_uri}
Enter this code:                 {user_code}
```

The user logs in to GitHub, enters the code, and approves. No code needed on your side — just wait.

### Step 3: Poll for the GitHub OAuth Token (`ghu_`, long-lived)

```rust
#[derive(Debug, Deserialize)]
struct AccessTokenResponse {
    access_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
    interval: Option<u64>,
}

/// Polls GitHub until the user authorizes or the code expires.
/// Returns the long-lived `ghu_` GitHub App user token on success.
/// This token does NOT expire (unless unused for 1 year or revoked).
async fn poll_for_access_token(
    client: &reqwest::Client,
    device_code: &str,
    interval: u64,
    expires_in: u64,
) -> anyhow::Result<String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(expires_in);

    loop {
        if std::time::Instant::now() > deadline {
            anyhow::bail!("Device code expired");
        }

        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;

        let resp: AccessTokenResponse = client
            .post("https://github.com/login/oauth/access_token")
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({
                "client_id": CLIENT_ID,
                "device_code": device_code,
                "grant_type": "urn:ietf:params:oauth:grant-type:device_code"
            }))
            .send()
            .await?
            .json()
            .await?;

        if let Some(token) = resp.access_token {
            return Ok(token);
        }

        match resp.error.as_deref() {
            Some("authorization_pending") => continue,
            Some("slow_down") => {
                tokio::time::sleep(std::time::Duration::from_secs(
                    resp.interval.unwrap_or(interval) + 5
                )).await;
                continue;
            }
            Some("expired_token") => anyhow::bail!("Device code expired"),
            Some(err) => anyhow::bail!("Auth error: {} — {}", err, resp.error_description.unwrap_or_default()),
            None => continue,
        }
    }
}
```

### Step 4: Exchange `ghu_` Token for Copilot Session Token (25-min TTL, unlocks premium models)

This is the critical step that unlocks the **full model catalog** including
premium/preview models. The raw `ghu_` token alone only gets GA models (gpt-4o,
gpt-4o-mini, gpt-3.5-turbo). The exchanged **session token** (`tid=...`) is what
the Copilot API requires for frontier model access.

```rust
#[derive(Debug, Deserialize)]
struct CopilotTokenResponse {
    token: String,               // Session token (often tid=... prefixed)
    expires_at: i64,             // Unix timestamp (~25-30 min)
    #[serde(default)]
    endpoints: Endpoints,
}

#[derive(Debug, Default, Deserialize)]
struct Endpoints {
    #[serde(default)]
    api: Option<String>,          // Correct API base URL for this account type
    #[serde(default)]
    proxy: Option<String>,
}

/// Exchanges the long-lived ghu_ token for a short-lived Copilot session token.
/// The session token is REQUIRED for premium/preview model access.
/// Re-exchange whenever needed — the ghu_ token never expires.
async fn get_copilot_token(
    client: &reqwest::Client,
    github_token: &str,
) -> anyhow::Result<CopilotTokenResponse> {
    let resp = client
        .get("https://api.github.com/copilot_internal/v2/token")
        .header("Authorization", format!("token {}", github_token))
        .header("Accept", "application/json")
        .header("User-Agent", "GitHubCopilotChat/0.26.7")
        .header("Editor-Version", "vscode/1.99.3")
        .header("Editor-Plugin-Version", "copilot-chat/0.26.7")
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("Token exchange failed: HTTP {}", resp.status());
    }

    Ok(resp.json().await?)
}
```

The `endpoints.api` field gives you the **account-specific** API base URL (e.g.,
`api.individual.githubcopilot.com` for Individual Pro,
`api.business.githubcopilot.com` for Business). Fall back to
`https://api.githubcopilot.com` if absent:

```rust
let api_base = copilot_token.endpoints.api
    .unwrap_or_else(|| "https://api.githubcopilot.com".to_string());
```

### Optional: Discover Account Type and Endpoint at Runtime

```rust
#[derive(Debug, Deserialize)]
struct CopilotUser {
    copilot_plan: String,          // "individual", "business", "enterprise"
    endpoints: CopilotEndpoints,
}

#[derive(Debug, Default, Deserialize)]
struct CopilotEndpoints {
    #[serde(default)]
    api: Option<String>,
    #[serde(default)]
    proxy: Option<String>,
}

/// Discovers the account's Copilot plan and correct API endpoint.
/// Works for Individual, Business, and Enterprise accounts.
async fn resolve_account_info(
    client: &reqwest::Client,
    github_token: &str,
) -> anyhow::Result<CopilotUser> {
    let resp = client
        .get("https://api.github.com/copilot_internal/user")
        .header("Authorization", format!("token {}", github_token))
        .header("User-Agent", "GitHubCopilotChat/0.26.7")
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("Failed to resolve account info: HTTP {}", resp.status());
    }

    Ok(resp.json().await?)
}
```

For Individual Pro subscribers, the endpoint is
`api.individual.githubcopilot.com` and the raw `ghu_` token may work directly as
Bearer without the exchange step. For Business/Enterprise, the exchanged session
token is required [1][2].

### Token Caching Strategy

Store both tokens on disk:

* `ghu_` **token** — long-lived, persists indefinitely (use at least once a year
  to survive GitHub's inactivity revocation). Save it once, never update it.

* **Copilot session token** — short-lived (25–30 min TTL), derived from the
  `ghu_` token. Cache it and refresh before expiry.

```rust
use std::path::PathBuf;

fn token_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("myapp")
}

fn github_token_path() -> PathBuf {
    token_dir().join("github_token")
}

fn copilot_token_cache_path() -> PathBuf {
    token_dir().join("copilot_token_cache.json")
}

/// Load cached Copilot session token if still valid (with 5-min buffer).
fn load_cached_copilot_token() -> Option<CopilotTokenResponse> {
    let path = copilot_token_cache_path();
    let data: CopilotTokenResponse = serde_json::from_reader(
        std::fs::File::open(&path).ok()?
    ).ok()?;
    let now = chrono::Utc::now().timestamp();
    if data.expires_at > now + 300 {
        Some(data)
    } else {
        None
    }
}

fn save_copilot_token(token: &CopilotTokenResponse) -> anyhow::Result<()> {
    std::fs::create_dir_all(token_dir())?;
    serde_json::to_writer(
        std::fs::File::create(copilot_token_cache_path())?,
        token,
    )?;
    Ok(())
}
```

### Required Headers for Chat Completions (MANDATORY for session tokens)

When using an exchanged session token, these headers are **required** — without
them, the API returns `403 token not authorized for this integration`. With a
raw `ghu_` token (no exchange), GA models work without headers, but premium
models are rejected regardless [3].

| Header | Value | Required for session token? |
| --- | --- | --- |
| `Authorization` | `Bearer {session_token}` | Yes |
| `Content-Type` | `application/json` | Yes |
| `Copilot-Integration-Id` | `vscode-chat` | **Yes** (without it: 403) |
| `Editor-Version` | `vscode/1.99.3` | **Yes** (without it: 403) |
| `Editor-Plugin-Version` | `copilot-chat/0.26.7` | **Yes** (without it: 403) |
| `User-Agent` | `GitHubCopilotChat/0.26.7` | Recommended |
| `Openai-Intent` | `conversation-panel` | Recommended |
| `x-github-api-version` | `2025-04-01` | Recommended |
| `X-Initiator` | `user` or `agent` (based on message roles) | Recommended |

### List Available Models (with session token for full catalog)

```rust
#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Vec<Model>,
}

#[derive(Debug, Deserialize)]
struct Model {
    id: String,
    #[serde(default)]
    owned_by: Option<String>,
}

async fn list_models(
    client: &reqwest::Client,
    copilot_token: &str,
    api_base: &str,
) -> anyhow::Result<Vec<Model>> {
    let resp: ModelsResponse = client
        .get(format!("{}/models", api_base))
        .header("Authorization", format!("Bearer {}", copilot_token))
        .header("Copilot-Integration-Id", "vscode-chat")
        .header("Editor-Version", "vscode/1.99.3")
        .header("Editor-Plugin-Version", "copilot-chat/0.26.7")
        .header("User-Agent", "GitHubCopilotChat/0.26.7")
        .header("Accept", "application/json")
        .send()
        .await?
        .json()
        .await?;
    Ok(resp.data)
}
```

Using the session token + integration headers returns the **full model catalog**
available to your account, including premium/preview models (GPT-5.x, Claude
Opus/Sonnet, Gemini Pro, etc.). Using a raw token without the exchange returns
only a limited set of GA models [2][3].

### Chat Completion with Tool Calling

The endpoint is OpenAI-compatible. Send `tools` in the request body (OpenAI
function-calling format). The model returns `tool_calls` in the response if it
decides to call a function. Your app executes the tools and feeds results back.

```rust
use serde_json::Value;

async fn chat_completions(
    client: &reqwest::Client,
    copilot_token: &str,
    api_base: &str,
    messages: &[Value],
    tools: &[Value],
    model: &str,
) -> anyhow::Result<Value> {
    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": false,
    });

    if !tools.is_empty() {
        body["tools"] = serde_json::json!(tools);
    }

    let resp = client
        .post(format!("{}/chat/completions", api_base))
        .header("Authorization", format!("Bearer {}", copilot_token))
        .header("Content-Type", "application/json")
        .header("Copilot-Integration-Id", "vscode-chat")
        .header("Editor-Version", "vscode/1.99.3")
        .header("Editor-Plugin-Version", "copilot-chat/0.26.7")
        .header("User-Agent", "GitHubCopilotChat/0.26.7")
        .header("Openai-Intent", "conversation-panel")
        .header("x-github-api-version", "2025-04-01")
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Chat completions failed: HTTP {} — {}", status, text);
    }

    Ok(resp.json().await?)
}
```

### Example Agent Loop

```rust
async fn agent_loop(
    client: &reqwest::Client,
    copilot_token: &str,
    api_base: &str,
    user_prompt: &str,
    tools: &[Value],
) -> anyhow::Result<String> {
    let mut messages: Vec<Value> = vec![
        serde_json::json!({
            "role": "user",
            "content": user_prompt
        })
    ];

    loop {
        let response = chat_completions(
            client, copilot_token, api_base, &messages, tools, "gpt-4o"
        ).await?;

        let choice = &response["choices"][0];
        let message = &choice["message"];

        // Check for tool calls
        if let Some(tool_calls) = message.get("tool_calls") {
            // Add the assistant message with tool_calls to history
            messages.push(message.clone());

            // YOUR app executes each tool call
            for tool_call in tool_calls.as_array().unwrap() {
                let tool_name = tool_call["function"]["name"].as_str().unwrap();
                let tool_args: Value = serde_json::from_str(
                    tool_call["function"]["arguments"].as_str().unwrap()
                )?;
                let tool_call_id = tool_call["id"].as_str().unwrap();

                // Execute the tool in YOUR app
                let result = my_app_execute_tool(tool_name, &tool_args).await;

                // Feed the tool result back to the model
                messages.push(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": tool_call_id,
                    "content": result
                }));
            }
            // Continue the loop — model will see tool results and respond
            continue;
        }

        // No tool calls — this is the final text response
        let content = message["content"]
            .as_str()
            .unwrap_or("(no content)")
            .to_string();

        return Ok(content);
    }
}

/// Your app's tool execution — implement whatever your app does
async fn my_app_execute_tool(name: &str, args: &Value) -> String {
    match name {
        "get_weather" => {
            let city = args["city"].as_str().unwrap_or("unknown");
            format!("{{\"city\": \"{}\", \"temp\": 72, \"condition\": \"sunny\"}}", city)
        }
        _ => format!("{{\"error\": \"unknown tool: {}\"}}", name),
    }
}
```

### Tool Definition Format (OpenAI-compatible)

```rust
let tools = vec![
    serde_json::json!({
        "type": "function",
        "function": {
            "name": "get_weather",
            "description": "Get the current weather for a city",
            "parameters": {
                "type": "object",
                "properties": {
                    "city": {
                        "type": "string",
                        "description": "The city name"
                    }
                },
                "required": ["city"]
            }
        }
    })
];
```

### Streaming (Optional)

For streaming responses, set `"stream": true` in the request body. The response
is Server-Sent Events (SSE). Parse `data:` lines as JSON chunks. Each chunk has
the same shape as a non-streaming response but with `delta` content instead of
full `message`.
For a full Rust SSE parser, consider the `eventsource-stream` crate:

```toml
eventsource-stream = "0.2"
futures-util = "0.3"
```

### Full First-Run Setup (Interactive, ONE TIME ONLY)

```rust
async fn first_run_setup() -> anyhow::Result<String> {
    let client = reqwest::Client::new();

    // Step 1: Request device code
    let device_code = request_device_code(&client).await?;
    println!("Open this URL: {}", device_code.verification_uri);
    println!("Enter this code: {}", device_code.user_code);
    println!("Waiting for authorization...");

    // Step 3: Poll for long-lived ghu_ token
    let github_token = poll_for_access_token(
        &client,
        &device_code.device_code,
        device_code.interval,
        device_code.expires_in,
    ).await?;

    println!("Authenticated successfully! Token saved for future use.");

    // Save ghu_ token to disk (long-lived, never needs refreshing)
    std::fs::create_dir_all(token_dir())?;
    std::fs::write(github_token_path(), &github_token)?;

    Ok(github_token)
}
```

### Runtime: Get or Refresh Copilot Session Token (Non-Interactive)

```rust
async fn ensure_copilot_token(client: &reqwest::Client) -> anyhow::Result<CopilotTokenResponse> {
    // Check cache first
    if let Some(cached) = load_cached_copilot_token() {
        return Ok(cached);
    }

    // Load long-lived ghu_ token from disk
    let github_token = std::fs::read_to_string(github_token_path())
        .map_err(|_| anyhow::anyhow!("Not authenticated. Run first_run_setup() first."))?;

    // Exchange for fresh Copilot session token (25-30 min TTL)
    // This session token unlocks the FULL model catalog including premium models
    let token = get_copilot_token(client, &github_token).await?;
    save_copilot_token(&token)?;
    Ok(token)
}
```

### Putting It All Together

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let client = reqwest::Client::new();

    // One-time setup if not already authenticated
    if !github_token_path().exists() {
        first_run_setup().await?;
    }

    // Get Copilot session token (cached, refreshes automatically)
    // This token unlocks premium/frontier models
    let copilot_token = ensure_copilot_token(&client).await?;
    let api_base = copilot_token.endpoints.api
        .unwrap_or_else(|| "https://api.githubcopilot.com".to_string());

    // List ALL available models (including premium, with session token + headers)
    let models = list_models(&client, &copilot_token.token, &api_base).await?;
    println!("Available models: {:?}", models.iter().map(|m| &m.id).collect::<Vec<_>>());

    // Define tools (OpenAI function-calling format)
    let tools = vec![
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "get_weather",
                "description": "Get current weather for a city",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "city": {"type": "string", "description": "City name"}
                    },
                    "required": ["city"]
                }
            }
        })
    ];

    // Run agent loop — YOUR app handles all tool calls
    // Use any model from the full catalog: gpt-5.4, claude-sonnet-4.5, gemini-3-pro, etc.
    let response = agent_loop(
        &client,
        &copilot_token.token,
        &api_base,
        "What's the weather in Seattle and Tokyo?",
        &tools,
    ).await?;

    println!("Agent response: {}", response);
    Ok(())
}
```

### Important Notes

* **Token expiry**: The `ghu_` OAuth token is long-lived and does NOT expire
  (unless unused for 1 year or revoked). The Copilot session token expires after
  ~25–30 minutes but can be re-derived from the `ghu_` token at any time without
  browser interaction.

* **Browser flow is one-time only**: You only need the interactive device flow
  once. After that, the `ghu_` token persists indefinitely. This matches the
  behavior of tools like OpenCode, LiteLLM, and Aider — they all authenticate
  once and never ask again.

* **Premium models require token exchange**: A raw `ghu_` token alone only gets
  GA models (gpt-4o, gpt-4o-mini, gpt-3.5-turbo). The exchanged **session token**
  (`tid=...`) + integration headers are **required** for frontier models
  (GPT-5.x, Claude Opus/Sonnet, Gemini Pro). Without the headers, the session
  token gets `403 token not authorized`.

* **Account-specific endpoints**: The `copilot_internal/v2/token` response's
  `endpoints.api` field gives the correct base URL for your account type
  (Individual → `api.individual.githubcopilot.com`, Business →
  `api.business.githubcopilot.com`, Enterprise → custom subdomain).

* **Client ID matters**: Use `Iv1.b507a08c87ecfe98` (GitHub App, produces `ghu_`
  tokens). OAuth App client IDs produce `gho_` tokens that cannot be exchanged at
  `copilot_internal/v2/token` (returns 404) and only access GA models.

* **Rate limits**: GitHub may throttle excessive requests. Suggested: 1 request
  every 2 seconds. Excessive automated/bulk requests can trigger GitHub's
  abuse-detection systems and result in temporary suspension of Copilot access.

* **Tool calling support**: The Copilot API is OpenAI-compatible but has not been
  officially documented for tool calling. Test early with a simple `tools`
  request to confirm the proxy layer passes them through.

* **Undocumented API**: `api.githubcopilot.com` (and account-specific variants)
  are technically undocumented for public use. They may change without notice.
  Use at your own risk.

### Quick Verification with curl

Before writing Rust code, verify the full flow works:

```bash
# Step 1: Request device code (uses GitHub App client ID Iv1.b507...)
curl -X POST 'https://github.com/login/device/code' \
  -H 'accept: application/json' \
  -H 'content-type: application/json' \
  --compressed \
  -d '{"client_id":"Iv1.b507a08c87ecfe98","scope":"read:user"}'

# Step 3: Poll for ghu_ token (replace DEVICE_CODE)
curl -X POST 'https://github.com/login/oauth/access_token' \
  -H 'accept: application/json' \
  -H 'content-type: application/json' \
  --compressed \
  -d '{"client_id":"Iv1.b507a08c87ecfe98","device_code":"DEVICE_CODE","grant_type":"urn:ietf:params:oauth:grant-type:device_code"}'

# Optional: Discover account type and correct endpoint
curl -s -H "Authorization: token GHO_TOKEN" \
  -H "User-Agent: GitHubCopilotChat/0.26.7" \
  https://api.github.com/copilot_internal/user | jq '{copilot_plan, endpoints}'

# Step 4: Exchange ghu_ for Copilot session token (unlocks premium models)
# Note: uses "token" auth scheme, not "Bearer"
curl -X GET 'https://api.github.com/copilot_internal/v2/token' \
  -H 'authorization: token GHU_TOKEN' \
  -H 'accept: application/json' \
  -H 'User-Agent: GitHubCopilotChat/0.26.7' \
  -H 'Editor-Version: vscode/1.99.3' \
  -H 'Editor-Plugin-Version: copilot-chat/0.26.7' \
  --compressed

# List all models (including premium) with session token + integration headers
curl -s 'https://api.githubcopilot.com/models' \
  -H "Authorization: Bearer SESSION_TOKEN" \
  -H "Copilot-Integration-Id: vscode-chat" \
  -H "Editor-Version: vscode/1.99.3" \
  -H "Editor-Plugin-Version: copilot-chat/0.26.7"

# Chat with tools and a premium model (replace SESSION_TOKEN)
curl -X POST 'https://api.githubcopilot.com/chat/completions' \
  -H "Authorization: Bearer SESSION_TOKEN" \
  -H "Copilot-Integration-Id: vscode-chat" \
  -H "Editor-Version: vscode/1.99.3" \
  -H "Editor-Plugin-Version: copilot-chat/0.26.7" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "claude-sonnet-4.5",
    "messages": [{"role": "user", "content": "What is the weather in Seattle?"}],
    "tools": [{"type": "function", "function": {"name": "get_weather", "description": "Get weather", "parameters": {"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]}}}]
  }'
```

### References

* [1] Copilot provider Individual vs Business endpoint: [https://github.com/NousResearch/hermes-agent/issues/27836](https://github.com/NousResearch/hermes-agent/issues/27836)

* [2] Copilot provider token exchange and enterprise endpoints: [https://github.com/NousResearch/hermes-agent/issues/7731](https://github.com/NousResearch/hermes-agent/issues/7731)

* [3] Copilot preview models require session token + headers: [https://github.com/anomalyco/opencode/issues/19338](https://github.com/anomalyco/opencode/issues/19338)

* [4] Supported AI models in GitHub Copilot: [https://docs.github.com/copilot/reference/ai-models/supported-models](https://docs.github.com/copilot/reference/ai-models/supported-models)

* [5] OAuth device flow & token persistence: [https://github.com/Aider-AI/aider/issues/2227](https://github.com/Aider-AI/aider/issues/2227)

* [6] GitHub OAuth device flow docs: [https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps](https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps)

* [7] Token expiration & revocation: [https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/token-expiration-and-revocation](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/token-expiration-and-revocation)

* [8] LiteLLM GitHub Copilot provider config: [https://docs.litellm.ai/docs/providers/github_copilot](https://docs.litellm.ai/docs/providers/github_copilot)

* [9] Copilot API headers (claude-code-router): [https://github.com/musistudio/claude-code-router/issues/119](https://github.com/musistudio/claude-code-router/issues/119)

* [10] PAT rejection evidence: [https://github.com/openclaw/openclaw/issues/52322](https://github.com/openclaw/openclaw/issues/52322)

* [11] Copilot CLI auth docs: [https://docs.github.com/en/copilot/how-tos/copilot-cli/set-up-copilot-cli/authenticate-copilot-cli](https://docs.github.com/en/copilot/how-tos/copilot-cli/set-up-copilot-cli/authenticate-copilot-cli)

* [12] Copilot CLI programmatic reference: [https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-programmatic-reference](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-programmatic-reference)
