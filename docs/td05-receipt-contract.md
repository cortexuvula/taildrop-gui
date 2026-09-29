# TD-05 Enriched Transfer-Record IPC Contract (v1, draft for review)

Status: **proposed** — published to @ada for stubbing before any TD-05 backend
code is written. Backend branch with the P1 fixes (TD-02/TD-01/TD-04) is
`fix/p1-receive-correctness` @ `efeafc5`; that PR does **not** implement this
contract. Nothing here is final until @codie reviews it against the P1 PR.

## Principles

1. **A receipt is a fact, not a view.** Once a file has been durably received,
   the record survives every subsequent empty poll. Receipts are never
   re-derived from "the inbox is empty right now".
2. **Paths come from the backend.** `savedPath` is the collision-resolved
   absolute path the content *actually landed at* (e.g. `report (1).pdf`), the
   same value `accept_file`/`accept_file_with_getter` already return. The
   frontend must never reconstruct a path from the requested filename.
3. **Errors are terminal states, not transient.** A failed receive stays in the
   list with `error` set until dismissed or retried; it is not replaced by the
   next poll's result.
4. **Exactly-once semantics per event.** Each receipt is delivered once; the
   replay buffer exists for reconnects (minimized window, refocus), not for
   duplication.

## Event: `transfer-receipt` (backend → frontend, new)

Emitted whenever a receive completes (auto-accept loop or manual accept) or
terminally fails. Payload:

```jsonc
{
  "id": "recv-1727654321000-a1b2c3",   // backend-generated, stable, unique
  "filename": "report.pdf",            // name as it appeared in the inbox
  "savedName": "report (1).pdf",       // name it landed under (collision-resolved)
  "savedPath": "/Users/x/Downloads/report (1).pdf", // absolute, from accept's return
  "size": 1048576,                      // bytes on disk after completion
  "peerName": "studio-mac",            // when the daemon exposes it, else null
  "direction": "received",
  "status": "saved" | "failed",
  "error": "Truncated download: received 3 of 10 advertised bytes...", // failed only
  "timestamp": 1727654321000            // ms since epoch, completion time
}
```

Frontend merges by `id` (dedupe), not by filename — double-accepts and
collisions intentionally produce distinct receipts.

## Event changes to existing payloads

- `incoming-files-changed` — unchanged (still the *pending* inbox snapshot).
- `incoming-files-error` — unchanged in shape; with TD-04 landed it now
  actually fires for CLI/HTTP receive failures, so the existing banner wiring
  is already correct.

## Command: `get_recent_receipts` (frontend → backend, new)

Returns receipts newer than a cursor, plus the cursor to persist:

```jsonc
// invoke("get_recent_receipts", { sinceMs: 1727654000000, limit: 50 })
{
  "receipts": [ /* TransferReceipt as above, newest first */ ],
  "nextCursorMs": 1727654321000
}
```

Called on mount and on window refocus. `nextCursorMs` is what the frontend
stores; passing it as the next `sinceMs` yields exactly the missed events.
Receipts older than 24h may be pruned by the backend.

## Retry affordance (failed receipts)

Failed receipts carry enough to retry: `filename` + the inbox entry still
existing (TD-01/TD-02 fixes preserve it now). Retry = existing
`accept_file` invoke with the current save dir. No new command needed in v1;
if we later want backend-side retry queue, that's v2.

## Out of scope for v1 (flag now, not later)

- Upload (send) receipts — same shape would work; defer until receive side
  proves out.
- Progress events — no measured progress exists on the receive path; per
  @ui-consultant, do not invent one.
- Cross-launch persistence — receipts live in backend memory for the session;
  persisting to disk is a separate decision.
