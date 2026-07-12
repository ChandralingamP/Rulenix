# Production Security

## Configuration

Set `APP_ENV=production` only with injected secrets. Startup fails unless:

- `DATABASE_URL` uses PostgreSQL credentials and TLS (`sslmode=verify-full` or `sslmode=require`).
- `CREDENTIAL_ENCRYPTION_KEYS`, `CREDENTIAL_ENCRYPTION_PRIMARY_VERSION`, and `OTP_HASH_KEY` are present and non-placeholder.
- SMTP host, username, password, and a parseable `SMTP_FROM` mailbox are present.
- `FRONTEND_ORIGINS` contains only concrete HTTPS origins.
- Angel One public IP, local IP, and MAC address are configured and non-default.

Use the environment-specific templates under `backend/.env.*.example`. Do not copy production secrets into files that are committed or baked into images.

## TLS Termination

Terminate TLS at Caddy, Nginx, a cloud load balancer, or Kubernetes ingress. Forward only HTTPS traffic to the frontend container. Forward `X-Forwarded-For`, `X-Forwarded-Proto`, and `Host`; configure `TRUSTED_PROXIES` to the proxy CIDRs so client IP rate limiting trusts only those proxies.

The backend adds CSP, HSTS in production, frame denial, MIME sniffing prevention, referrer, and permissions-policy headers. The frontend Nginx config adds the same baseline headers for static assets.

## Secret Injection

Preferred sources:

- Docker secrets, Kubernetes Secrets mounted as environment variables, systemd `EnvironmentFile` owned by root, or a cloud secret manager.
- A distinct 32-byte credential encryption key per environment.
- A distinct OTP hash key per environment.

Never inject MPINs, TOTPs, broker tokens, SMTP passwords, or encryption keys through command-line arguments because they can appear in process listings.

## OTPs

API responses do not return OTP values. In development, missing SMTP causes OTP delivery to be logged for local testing. Production validation requires SMTP so OTP values are not logged as a fallback.
