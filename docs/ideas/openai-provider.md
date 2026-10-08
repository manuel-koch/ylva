# OpenAI provider

Add a direct provider implementation for the OpenAI API. It is not implemented
as a named provider connection today. Ylva currently supports OpenRouter and
Ollama through the shared OpenAI-compatible chat transport; OpenAI models may
be reached through OpenRouter, but that is not a direct OpenAI integration.
