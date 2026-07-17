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

    /// A panic inside the offloaded closure surfaces as `Internal`, not as a lost error.
    #[tokio::test]
    async fn panic_in_offloaded_task_maps_to_internal() {
        let joined = tokio::task::spawn_blocking(|| panic!("boom")).await;
        let err = spawn_join_err(joined.expect_err("the task panicked, so join must fail"));
        match err {
            MemoryError::Internal(msg) => assert!(
                msg.starts_with("offloaded task failed:"),
                "message must name the offload as the failure: {msg}"
            ),
            other => panic!("expected Internal, got {other:?}"),
        }
    }

    /// The happy path does not route through here at all — a closure's own `Ok` is
    /// returned untouched. Pins that `spawn_join_err` is only the *join*-failure path.
    #[tokio::test]
    async fn successful_offload_never_produces_a_join_error() {
        let joined = tokio::task::spawn_blocking(|| 42).await;
        assert_eq!(joined.expect("a non-panicking task joins cleanly"), 42);
    }
}
