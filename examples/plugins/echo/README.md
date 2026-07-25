# Example plugin: `echo`

A minimal **third-party plugin** for the Wovyr platform, demonstrating the Plugin SDK
end to end ([docs/08-plugin-sdk](../../../docs/08-plugin-sdk/overview.md)): a signed
package is built → installed → granted → enabled → **used**.

The plugin contributes one `tool` capability, `echo.run`, backed by a `wasm32-wasi`
module ([`echo.wasm`](echo.wasm), source [`echo.wat`](echo.wat)). It implements the
plugin capability ABI in its simplest form: **read the JSON request from stdin, write
it back as the JSON response on stdout.**

## Files

| File | Purpose |
|------|---------|
| `plugin.yaml` | Manifest: identity, platform-API range, requested permission, the `echo.run` tool capability, and the digest-pinned `echo.wasm` artifact. |
| `echo.wat` / `echo.wasm` | Capability source and its compiled module (`wat2wasm echo.wat -o echo.wasm`). |

## Walk-through

The capability *runs* only when the CLI is built with the WASM loader:

```bash
cargo build -p wovyr-cli --features plugin-wasi
alias wovyr=target/debug/wovyr
export WOVYR_LOG=warn          # quiet the sandbox's trace logging
```

1. **Generate a publisher signing key** (the plugin author does this once):

   ```bash
   wovyr plugin keygen acme --dir ./keys
   ```

2. **Trust the publisher** (the operator installing the plugin):

   ```bash
   wovyr plugin trust acme --key ./keys/acme.pub
   ```

3. **Sign the package** — the detached signature covers the manifest bytes:

   ```bash
   wovyr plugin sign --key ./keys/acme.key --manifest examples/plugins/echo/plugin.yaml
   # writes examples/plugins/echo/plugin.sig
   ```

4. **Install**, granting the permission the manifest requests (install is fail-closed
   on an untrusted publisher, a bad signature, an artifact digest mismatch, or an
   ungranted permission):

   ```bash
   wovyr plugin install examples/plugins/echo --grant 'net:egress:api.example.com'
   wovyr plugin list
   ```

5. **Enable** — the capability goes live and is registered into the tool registry:

   ```bash
   wovyr plugin enable acme/echo
   ```

6. **Use it** — invoke the capability directly through the engine's WASM runtime
   (`plugin run` is an operator test path; an agent calls the same registered tool):

   ```bash
   wovyr plugin run echo.run --input '{"message":"hello from a plugin","n":7}'
   # => { "message": "hello from a plugin", "n": 7 }
   ```

   Enabled plugin tools are also wired into `wovyr agents run --local`, so an agent whose
   model emits a `echo.run` tool call invokes the same capability.

## Distributing as a single file

Bundle the signed package directory into one content-addressed `.wovyrpkg` and install
from it directly (distribution §2 / §5 "Local file"):

```bash
wovyr plugin pack examples/plugins/echo --out echo.wovyrpkg
wovyr plugin install echo.wovyrpkg --grant 'net:egress:api.example.com'
```

## Lifecycle extras

```bash
wovyr plugin disable acme/echo      # withdraw the capability (state retained)
wovyr plugin uninstall acme/echo    # remove it and its staged artifacts
```

Building a new version of `plugin.yaml` (bump `metadata.version`) lets you exercise
`wovyr plugin upgrade examples/plugins/echo` and `wovyr plugin rollback acme/echo`.

> A real plugin compiles its capability from a higher-level language (e.g. Rust →
> `wasm32-wasi`) and parses/produces structured JSON; `echo` keeps the module trivial so
> the focus stays on the **packaging, signing, trust, and lifecycle** flow.
