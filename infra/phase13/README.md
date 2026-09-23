# Phase 13 reversible API routing

`nginx-routing.conf.template` sends `/api/`, `/ws/`, `/health`, and
`/readiness` to one selected backend. The single `RULENIX_API_UPSTREAM`
value prevents split routing between runtimes.

Render and validate a candidate without changing the public listener:

```sh
docker run --rm \
  -e RULENIX_API_UPSTREAM=backend:8080 \
  -v "$PWD/infra/phase13/nginx-routing.conf.template:/etc/nginx/templates/default.conf.template:ro" \
  nginx:1.27-alpine nginx -t
```

Use `backend:8080` for Rust. Only after an explicitly approved authority
transfer may the same value be changed to `python-runtime:8080`. Rollback
first fences Python and restores Rust authority, then restores
`backend:8080`; never run two route selectors for different path groups.
