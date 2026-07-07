//! RM-GA-P2 DUR-403 acceptance: "concurrent audit appends from two handles
//! produce one chain that `verify()` accepts." Before this fix, `AuditLog`
//! cached the chain tip in memory per-instance; two `AuditLog`s over the same
//! `FileAuditSink` directory (the CLI and server both writing `~/.apex/audit`)
//! would each build entries on top of a stale tip, forking the chain — and
//! `verify()` would then report tampering that never actually happened.

use apex_audit::{AuditEvent, AuditLog, FileAuditSink};

fn ev(n: u64, action: &str) -> AuditEvent {
    AuditEvent::new(n, "alice", "acme", action, "secret", "secret://acme/token")
}

#[test]
fn concurrent_appends_from_independent_log_handles_produce_one_verifiable_chain() {
    let dir = std::env::temp_dir().join(format!(
        "apex_audit_concurrent_append_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    const HANDLES: usize = 6;
    const RECORDS_PER_HANDLE: usize = 20;

    let handles: Vec<_> = (0..HANDLES)
        .map(|h| {
            let dir = dir.clone();
            std::thread::spawn(move || {
                // A fresh, independent AuditLog per thread — the stand-in for
                // a separate process (the CLI vs. the server) both writing
                // the same `audit.jsonl`.
                let sink = FileAuditSink::new(&dir).unwrap();
                let log = AuditLog::open_with_lock(Box::new(sink), &dir).unwrap();
                for i in 0..RECORDS_PER_HANDLE {
                    log.record(ev((h * 1000 + i) as u64, "secret.read"))
                        .unwrap();
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }

    // A fresh handle reads back the merged history: every record from every
    // thread landed, sequence numbers are contiguous with no duplicates or
    // gaps, and the chain verifies — a fork would show up as either a
    // duplicate seq or a verify() failure.
    let sink = FileAuditSink::new(&dir).unwrap();
    let log = AuditLog::open_with_lock(Box::new(sink), &dir).unwrap();
    let all = log.query(&Default::default()).unwrap();

    let expected_total = HANDLES * RECORDS_PER_HANDLE;
    assert_eq!(all.len(), expected_total, "no append was lost");

    let mut seqs: Vec<u64> = all.iter().map(|e| e.seq).collect();
    seqs.sort_unstable();
    let expected: Vec<u64> = (0..expected_total as u64).collect();
    assert_eq!(
        seqs, expected,
        "sequence numbers must be exactly contiguous 0..N — a fork would \
         produce duplicates or gaps"
    );

    log.verify()
        .expect("one chain extended by every writer, not forked, must verify cleanly");

    let _ = std::fs::remove_dir_all(&dir);
}
