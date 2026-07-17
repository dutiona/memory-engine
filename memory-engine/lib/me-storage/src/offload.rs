//! The blocking-offload error contract.
//!
//! Every consumer of this port reaches storage through `tokio::task::spawn_blocking`
//! (the engine is async-native, #631/#702, while the `SQLite` backend is synchronous).
//! That offload can fail in exactly one way that is not the store's fault: the joined
//! task panicked or was cancelled, and tokio hands back a [`JoinError`] instead of the
//! closure's own `Result`.
//!
//! [`spawn_join_err`] is the single mapping from that failure to a [`MemoryError`]. It
//! lives here, in the port, because the port is what defines the offload contract —
//! not in any one primitive that happens to use it.
//!
//! Before Wave 2 #816 / S5 it existed as **five byte-identical private copies** (facade,
//! `me-ingest`, `me-query`, `me-consolidate`, `me-cognitive`). That was not an accident:
//! an L3 primitive may not depend on the facade, and until the carves were done no lower
//! layer offered a home. The carves are done; the reason to duplicate has expired (#984).

use me_types::error::MemoryError;
use tokio::task::JoinError;

/// Map a `spawn_blocking` join failure — a panic or cancellation **inside** the offloaded
/// closure — to a [`MemoryError`].
///
/// This is *not* the store's own error path: a closure that returns `Err(MemoryError)`
/// propagates that error unchanged. A [`JoinError`] means the task never produced a
/// result at all, so there is nothing more specific to report than that the offload
/// itself failed.
///
/// # Examples
///
/// ```
/// use me_storage::spawn_join_err;
///
/// # async fn demo() -> Result<(), me_types::error::MemoryError> {
/// let value = tokio::task::spawn_blocking(|| 21 * 2)
///     .await
///     .map_err(spawn_join_err)?;
/// assert_eq!(value, 42);
/// # Ok(())
/// # }
/// # tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(demo()).unwrap();
/// ```
#[must_use]
#[allow(
    clippy::needless_pass_by_value,
    reason = "used as map_err(spawn_join_err) fn pointer"
)]
pub fn spawn_join_err(e: JoinError) -> MemoryError {
    MemoryError::Internal(format!("offloaded task failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A panic inside the offloaded closure surfaces as `Internal`, carrying the **exact**
    /// formatted message.
    ///
    /// Asserted against the full `format!` output rather than a prefix: a prefix check
    /// stays green under a mutation that drops or garbles the `JoinError`'s own text
    /// (e.g. `Internal("offloaded task failed: wrong")`), which is precisely the detail
    /// that makes the error diagnosable. Building the expectation from the *same*
    /// `JoinError` the mapper receives keeps the test honest without hard-coding tokio's
    /// wording, which is not ours to pin.
    #[tokio::test]
    async fn panic_in_offloaded_task_maps_to_internal_with_the_exact_message() {
        let joined = tokio::task::spawn_blocking(|| panic!("boom")).await;
        let join_err = joined.expect_err("the task panicked, so join must fail");
        let expected = format!("offloaded task failed: {join_err}");

        match spawn_join_err(join_err) {
            MemoryError::Internal(msg) => assert_eq!(msg, expected),
            other => panic!("expected Internal, got {other:?}"),
        }
    }

    /// A **cancelled** task is the other way a join fails, and it must map the same way —
    /// not silently produce a different variant.
    #[tokio::test]
    async fn cancelled_task_also_maps_to_internal() {
        let handle = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        });
        handle.abort();
        let join_err = handle.await.expect_err("an aborted task must fail to join");
        assert!(
            join_err.is_cancelled(),
            "precondition: this is the cancel path"
        );

        match spawn_join_err(join_err) {
            MemoryError::Internal(msg) => assert!(
                msg.starts_with("offloaded task failed: "),
                "cancellation must be reported as an offload failure: {msg}"
            ),
            other => panic!("expected Internal, got {other:?}"),
        }
    }

    /// The mapper is **only** the join-failure path: a closure's own `Err` propagates
    /// unchanged and never reaches `spawn_join_err`.
    ///
    /// This is what stops the mapper from swallowing a real store error into a generic
    /// `Internal` — the distinction the module doc claims and nothing else tested.
    #[tokio::test]
    async fn a_closures_own_error_propagates_untouched() {
        let joined: Result<Result<(), MemoryError>, _> =
            tokio::task::spawn_blocking(|| Err(MemoryError::NotFound("fact 7".into()))).await;

        let inner = joined
            .map_err(spawn_join_err)
            .expect("the task did not panic, so the join itself succeeds");

        match inner {
            Err(MemoryError::NotFound(what)) => assert_eq!(what, "fact 7"),
            other => panic!("the closure's own error must survive verbatim, got {other:?}"),
        }
    }
}
