import { createHash } from "node:crypto";

const bounded = (value, secrets = []) => {
  let text = String(value ?? "");
  for (const secret of secrets.filter(Boolean)) text = text.replaceAll(secret, "[REDACTED]");
  return text.replace(/[\u0000-\u001f\u007f]/g, " ").slice(0, 256);
};

export function credentialFingerprint(value) {
  if (!value) return "missing";
  return createHash("sha256").update(value).digest("hex").slice(0, 12);
}

export function brokerAccountRef(value) {
  const text = String(value ?? "").trim();
  return text ? `***${text.slice(-4)}` : "missing";
}

export function readDiagnostic({
  operation,
  path,
  method,
  expectedLocalAddress,
  response,
  secrets = [],
}) {
  const payload = response?.payload;
  return {
    semantic_operation: operation,
    path,
    method,
    expected_local_address: expectedLocalAddress ?? "default",
    actual_local_address: response?.localAddress ?? "unavailable",
    required_headers: {
      authorization_bearer: true,
      x_privatekey: true,
      x_usertype: true,
      x_sourceid: true,
      x_clientlocalip: true,
      x_clientpublicip: true,
      x_macaddress: true,
      accept_json: true,
      content_type_json: true,
    },
    http_status: Number(response?.httpStatus ?? 0),
    broker_status: typeof payload?.status === "boolean" ? payload.status : null,
    broker_error_code: bounded(payload?.errorcode, secrets),
    broker_message: bounded(payload?.message, secrets),
  };
}

export function strictAuthoritativeGate({ platformAllowed, unreadableAccounts, required }) {
  return Boolean(platformAllowed) && (!required || Number(unreadableAccounts) === 0);
}
