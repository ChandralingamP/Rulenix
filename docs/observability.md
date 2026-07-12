# Observability And Audit

## Logs

`APP_ENV=staging` and `APP_ENV=production` emit JSON logs. Every request receives `X-Request-ID` and `X-Correlation-ID`; clients can supply either header with a safe bounded value.

Never log credentials, cookies, authorization headers, MPINs, TOTPs, or broker token payloads. Broker request failures redact known submitted secrets.

## Metrics

`GET /api/metrics` returns a JSON operational snapshot:

- Active sessions
- Market feed age
- Scheduler run counts
- Order counts by status
- Broker errors in the last 24 hours
- Risk rejections in the last 24 hours
- Unhealthy reconciliation records

Connect this endpoint to your scraper or a lightweight synthetic monitor.

## Alerts

Operational alerts are persisted in `strategy_events` and delivered to `ALERT_WEBHOOK_URL` when configured. Delivery attempts are recorded in `alert_delivery_attempts`. Alert when feeds are stale, scheduler leadership is unavailable, reconciliation is unhealthy, broker sessions expire, protective orders fail, or the database health endpoint fails.

## Audit Trail

`audit_events` is append-only through database triggers. It records login, logout, admin permission changes, account profile changes, trading-mode changes, and strategy activation/configuration changes. Extend this table for any new privileged workflow.

## Retention

Suggested minimums:

- Application logs: 30 days hot, 180 days archived.
- Audit events: 1 year hot, 7 years archived if required by policy.
- Alert attempts: 90 days.
- Broker diagnostics: 90 days unless needed for an incident.

## Troubleshooting

1. Check `/api/health/ready`.
2. Check `/api/metrics` for feed age, reconciliation, and scheduler counts.
3. Search JSON logs by request ID or correlation ID.
4. Review `strategy_events` for deduplicated operational alerts.
5. Review `audit_events` for control-plane changes near the incident.
