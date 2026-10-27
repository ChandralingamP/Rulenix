# Angel One static egress IPs

Rulenix treats source-address selection as an optional Angel broker-account setting.

- `user_profiles.broker_egress_ip_id IS NULL` uses the existing unbound HTTP/WebSocket path. Linux chooses the source address and no production IP is hard-coded as a fallback.
- An assigned inventory row must be `CONFIGURED` and `VERIFIED`. REST clients use `reqwest::ClientBuilder::local_address`; WebSockets bind a `tokio::net::TcpSocket` before the TLS handshake.
- Any lookup, status, bind, connect, or verification failure for an explicit assignment blocks that broker operation. There is no retry through the default route.
- Clients are resolved per account and are not shared across users. Assignment changes therefore have no stale-client cache to invalidate.

## Restricted helper

`rulenix-egress-helper` is a root systemd service with a typed JSON Unix-socket protocol. The backend container can connect to the socket but cannot mutate the socket directory or run privileged commands. The helper:

1. accepts only `configure_and_verify` plus one validated globally routable IPv4;
2. uses fixed administrator-configured interface, state, netplan, socket, and verification URL values;
3. passes fixed argument arrays directly to `ip`, `netplan`, `docker`, `nsenter`, and `curl` without a shell;
4. adds missing `/32` addresses to the host interface and a helper-owned netplan fragment;
5. adds the `/32` to the backend container network namespace so application socket binding is real;
6. verifies the external source from that namespace;
7. never exposes an address-removal operation.

The helper keeps its managed address set in `/var/lib/rulenix-egress/managed-ipv4`, validates generated netplan before reporting success, and backs up its prior managed fragment. The existing cloud-init netplan file and default route are not edited.

## Production installation

The checked-in systemd unit is specific to the audited production interface `ens3`. Re-audit the interface and network manager before using it on another host.

```sh
docker build --target egress-helper-artifact -t rulenix-egress-helper-artifact ./backend
helper_container="$(docker create rulenix-egress-helper-artifact)"
docker cp "$helper_container:/usr/local/sbin/rulenix-egress-helper" /usr/local/sbin/rulenix-egress-helper
docker rm "$helper_container"
install -o root -g root -m 0755 infra/systemd/rulenix-egress-helper.service /etc/systemd/system/rulenix-egress-helper.service
systemctl daemon-reload
systemctl enable --now rulenix-egress-helper.service
```

The production Compose definition bind-mounts `/run/rulenix-egress` into the unprivileged backend container. The helper socket is owned by numeric UID `10001`, matching the backend image user, and mode `0600`.

## Rollback

Application rollback is forward-compatible: the migration only adds a table and nullable column. Stop traffic, restore the prior frontend/backend release, and restore the prior Compose definition. Existing accounts have NULL assignments until an administrator explicitly changes one.

Do not remove configured IPs during normal application rollback. To disable future automated configuration while retaining Linux addresses, stop and disable `rulenix-egress-helper.service`; assigned accounts will fail closed after a container restart because their namespace address cannot be rehydrated.

An emergency OS-network rollback is intentionally manual and must be approved separately because the feature's invariant is to never automatically remove configured IPs. Use the pre-change netplan backup and OVH recovery console, validate `netplan generate`, then apply during a maintenance window. Never change or remove the primary address/default route as part of that procedure.
