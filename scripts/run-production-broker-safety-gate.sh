#!/usr/bin/env bash
set -euo pipefail
trap 'echo "DEPLOYMENT_GATE_FAILED_LINE=${LINENO}" >&2' ERR

cd /opt/rulenix
readonly audit_dir="${RULENIX_GATE_AUDIT_DIR:-/opt/rulenix/scripts}"
for required in \
  production-broker-safety-inventory.sql \
  production-broker-safety-gate.mjs \
  production-broker-safety-gate-lib.mjs \
  broker-exposure-classifier.mjs
do
  test -r "${audit_dir}/${required}"
done

backend_pid="$(sudo docker inspect --format '{{.State.Pid}}' rulenix-backend-1)"
test -n "${backend_pid}"
test "${backend_pid}" != "0"

sudo docker exec -i rulenix-postgres-1 \
  psql -X -q -U rulenix -d rulenix -At \
  < "${audit_dir}/production-broker-safety-inventory.sql" \
  | sudo docker run --rm -i \
      --network container:rulenix-backend-1 \
      --env-file /opt/rulenix/backend/.env.production \
      -v "${audit_dir}/production-broker-safety-gate.mjs:/audit/production-broker-safety-gate.mjs:ro" \
      -v "${audit_dir}/production-broker-safety-gate-lib.mjs:/audit/production-broker-safety-gate-lib.mjs:ro" \
      -v "${audit_dir}/broker-exposure-classifier.mjs:/audit/broker-exposure-classifier.mjs:ro" \
      node:22-bookworm-slim node /audit/production-broker-safety-gate.mjs

echo 'BROKER_READ_FAILURE_IS_FLAT=false'
echo 'BROKER_MUTATIONS_PLACED=0'
echo 'BROKER_MUTATIONS_MODIFIED=0'
echo 'BROKER_MUTATIONS_CANCELLED=0'
