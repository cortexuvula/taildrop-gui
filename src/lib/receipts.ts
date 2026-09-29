import type {
  TransferReceipt,
  ReceiptPage,
  StagingRecoveryFoundEvent,
} from "../types";

/**
 * TD-05: Merge a new page of receipts into existing state.
 *
 * - Dedupes by `id` (collisions and double-accepts produce distinct receipts).
 * - Appends in ascending seq order.
 * - When a receipt with a matching `id` already exists, the newer version
 *   wins (e.g., a failed receipt recovered via staging emits a fresh "saved"
 *   receipt with the same id).
 */
export function mergeReceipts(
  prev: TransferReceipt[],
  page: ReceiptPage,
): TransferReceipt[] {
  const byId = new Map<string, TransferReceipt>();
  for (const r of prev) byId.set(r.id, r);
  for (const r of page.receipts) {
    const existing = byId.get(r.id);
    if (!existing || r.seq > existing.seq) {
      byId.set(r.id, r);
    }
  }
  return Array.from(byId.values()).sort((a, b) => a.seq - b.seq);
}

/**
 * TD-05: Filter receipts by status.
 */
export function savedReceipts(receipts: TransferReceipt[]): TransferReceipt[] {
  return receipts.filter((r) => r.status === "saved");
}

export function failedReceipts(receipts: TransferReceipt[]): TransferReceipt[] {
  return receipts.filter((r) => r.status === "failed");
}

/**
 * TD-05: Find the highest seq in a receipt list (for cursor resume).
 * Returns 0 when the list is empty.
 */
export function highestSeq(receipts: TransferReceipt[]): number {
  let max = 0;
  for (const r of receipts) if (r.seq > max) max = r.seq;
  return max;
}

/**
 * TD-05: Type-safe guard for the `staging-recovery-found` event payload.
 */
export function isStagingRecoveryEvent(payload: unknown): payload is StagingRecoveryFoundEvent {
  return (
    typeof payload === "object" &&
    payload !== null &&
    Array.isArray((payload as { dirs?: unknown }).dirs)
  );
}

/**
 * TD-05: Type-safe guard for `TransferReceipt` event payloads.
 */
export function isTransferReceipt(payload: unknown): payload is TransferReceipt {
  return (
    typeof payload === "object" &&
    payload !== null &&
    typeof (payload as { seq?: unknown }).seq === "number" &&
    typeof (payload as { id?: unknown }).id === "string" &&
    ((payload as { status?: unknown }).status === "saved" ||
      (payload as { status?: unknown }).status === "failed")
  );
}
