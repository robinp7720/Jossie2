# systemd

Sample unit files for running Jossie2 under `systemd`.

`jossie2.service` assumes a source checkout deployed at `/opt/jossie`:

- `config.toml` lives at `/opt/jossie/config.toml`
- the binary lives at `/opt/jossie/target/release/jossie2`
- the built web UI lives at `/opt/jossie/frontend/dist`

An optional environment file can override secrets and runtime settings:

```sh
/etc/jossie/jossie.env
```

Useful variables include:

- `JOSSIE_SERVER_AUTH_TOKEN`
- `JOSSIE_SERVER_PUBLIC_BASE_URL`
- `JOSSIE_LLM_API_KEY`
- `JOSSIE_LLM_SYSTEM_PROMPT`
- `JOSSIE_LLM_MAX_CONTEXT_MESSAGES`
- `JOSSIE_LLM_EVENT_MAX_CONTEXT_MESSAGES`
- `JOSSIE_TELEGRAM_BOT_TOKEN`
- `JOSSIE_EMAIL_USERNAME`
- `JOSSIE_EMAIL_PASSWORD`
- `JOSSIE_EMAIL_IMAP_HOST`
- `JOSSIE_EMAIL_SMTP_HOST`
- `JOSSIE_GOOGLE_CLIENT_ID`
- `JOSSIE_GOOGLE_CLIENT_SECRET`
- `JOSSIE_GOOGLE_REFRESH_TOKEN`
- `JOSSIE_LOG_JSON`
