# Local Rekor transparency log

A self-contained [Rekor](https://github.com/sigstore/rekor) stack (Rekor server +
Trillian log server/signer + MySQL + Redis) for developing and testing the platform's
keyless-signing slice against **real** transparency-log infrastructure, without any
dependency on the public `rekor.sigstore.dev`.

All five images are pinned released builds — nothing is compiled from source, so the
stack comes up on a cold machine with a single (resumable) image pull.

## Run

```bash
docker compose -f deployment/rekor/docker-compose.yml up -d --wait
curl -s http://localhost:3000/api/v1/log | jq .   # log info ⇒ healthy
```

The API listens on `localhost:3000`, Prometheus metrics on `localhost:2112`. MySQL,
Redis, and Trillian are internal to the compose network.

## Wiring into Apex

Capability-gated integration tests (the same pattern as `APEX_MEMORY_POSTGRES_URL` /
`APEX_FC_KERNEL`) should read the log's URL from:

```bash
export APEX_REKOR_URL=http://localhost:3000
```

and skip cleanly when it is unset.

## Caveats

- The log signer is **in-memory** (`--rekor_server.signer=memory`): the log's public
  key changes on every restart, so entries are disposable dev/test data — do not pin
  the key anywhere durable.
- `docker` socket access: after a daemon restart this box needs
  `sudo chmod 666 /var/run/docker.sock` for non-login shells (see repo dev notes).

## Teardown

```bash
docker compose -f deployment/rekor/docker-compose.yml down -v
```
