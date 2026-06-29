;; echo — a wasm32-wasi plugin capability for the Apex platform.
;; Reads the JSON request from stdin (fd 0) and writes it back verbatim to stdout
;; (fd 1): the simplest demonstration of the plugin capability ABI
;; (request JSON in → response JSON out). Build: wat2wasm echo.wat -o echo.wasm
(module
  (import "wasi_snapshot_preview1" "fd_read"
    (func $fd_read (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write"
    (func $fd_write (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "_start")
    ;; read iovec @0 -> {buf=100, len=900}
    (i32.store (i32.const 0) (i32.const 100))
    (i32.store (i32.const 4) (i32.const 900))
    (drop (call $fd_read (i32.const 0) (i32.const 0) (i32.const 1) (i32.const 8)))
    ;; write iovec @16 -> {buf=100, len=bytes_read}
    (i32.store (i32.const 16) (i32.const 100))
    (i32.store (i32.const 20) (i32.load (i32.const 8)))
    (drop (call $fd_write (i32.const 1) (i32.const 16) (i32.const 1) (i32.const 24)))))
