use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
#[cfg(test)]
use std::time::Duration;
use std::time::Instant;

use tokio::sync::oneshot;

pub const MESSAGE_RECEIPT_ID: &str = "message_receipt_id";

type ReceiptResult = std::result::Result<(), String>;
type ReceiptSender = oneshot::Sender<ReceiptResult>;

fn receipt_registry(
) -> &'static Mutex<HashMap<String, (Option<ReceiptSender>, Instant, Arc<ClaimState>)>> {
    static REGISTRY: OnceLock<
        Mutex<HashMap<String, (Option<ReceiptSender>, Instant, Arc<ClaimState>)>>,
    > = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn register_message_receipt() -> (String, oneshot::Receiver<ReceiptResult>) {
    let receipt_id = uuid::Uuid::new_v4().simple().to_string();
    let (sender, receiver) = oneshot::channel();
    let mut registry = receipt_registry()
        .lock()
        .expect("message receipt registry poisoned");
    let now = Instant::now();
    registry.retain(|_, (sender, _, state)| {
        if sender.as_ref().is_some_and(ReceiptSender::is_closed) {
            sender.take();
            state.cancelled.store(true, Ordering::SeqCst);
            state.notify.notify_waiters();
        }
        sender.is_some() || state.active.load(Ordering::SeqCst)
    });
    registry.insert(
        receipt_id.clone(),
        (Some(sender), now, Arc::new(ClaimState::default())),
    );
    (receipt_id, receiver)
}

pub fn complete_message_receipt(receipt_id: &str, result: ReceiptResult) -> bool {
    let mut registry = receipt_registry()
        .lock()
        .expect("message receipt registry poisoned");
    let Some((sender, _, state)) = registry.get_mut(receipt_id) else {
        return false;
    };
    let sender = sender.take();
    if !state.active.load(Ordering::SeqCst) {
        registry.remove(receipt_id);
    }
    sender.is_some_and(|sender| sender.send(result).is_ok())
}

#[derive(Default)]
struct ClaimState {
    active: AtomicBool,
    cancelled: AtomicBool,
    notify: tokio::sync::Notify,
}
pub struct ReceiptClaim {
    id: String,
    state: Arc<ClaimState>,
}
impl ReceiptClaim {
    pub async fn cancelled(&self) {
        loop {
            let notified = self.state.notify.notified();
            if self.state.cancelled.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }
}
impl Drop for ReceiptClaim {
    fn drop(&mut self) {
        let mut registry = receipt_registry().lock().expect("receipt registry");
        self.state.active.store(false, Ordering::SeqCst);
        if registry
            .get(&self.id)
            .is_some_and(|(sender, _, _)| sender.as_ref().is_none_or(ReceiptSender::is_closed))
        {
            registry.remove(&self.id);
        }
        self.state.notify.notify_waiters();
    }
}
pub fn claim_message_receipt(id: &str) -> Option<ReceiptClaim> {
    let registry = receipt_registry().lock().expect("receipt registry");
    let (sender, _, state) = registry.get(id)?;
    if sender.as_ref().is_none_or(ReceiptSender::is_closed)
        || state.active.swap(true, Ordering::SeqCst)
    {
        return None;
    }
    Some(ReceiptClaim {
        id: id.to_string(),
        state: state.clone(),
    })
}
/// A cancellation acknowledgement remains valid even after registry cleanup.
pub struct ReceiptCancellation(Arc<ClaimState>);
impl ReceiptCancellation {
    pub fn is_finished(&self) -> bool {
        !self.0.active.load(Ordering::SeqCst)
    }
    pub async fn wait(self) {
        loop {
            let notified = self.0.notify.notified();
            if self.is_finished() {
                break;
            }
            notified.await;
        }
    }
}

/// Revoke synchronously, retaining active work until its claim is dropped.
pub fn cancel_message_receipt_with_ack(id: &str) -> Option<ReceiptCancellation> {
    let mut registry = receipt_registry().lock().expect("receipt registry");
    let (sender, _, state) = registry.get_mut(id)?;
    sender.take();
    state.cancelled.store(true, Ordering::SeqCst);
    state.notify.notify_waiters();
    let cancellation = ReceiptCancellation(state.clone());
    if cancellation.is_finished() {
        registry.remove(id);
    }
    Some(cancellation)
}
pub fn cancel_message_receipt(id: &str) -> bool {
    cancel_message_receipt_with_ack(id).is_some()
}
/// Cancellation is acknowledged only after the runtime has dropped its turn future.
pub async fn cancel_message_receipt_and_wait(id: &str) {
    if let Some(cancellation) = cancel_message_receipt_with_ack(id) {
        cancellation.wait().await;
    }
}
/// Revoke queued work if its dispatch future is dropped.
pub struct MessageReceiptGuard(pub String);
impl Drop for MessageReceiptGuard {
    fn drop(&mut self) {
        cancel_message_receipt(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn active_receipt_keeps_acknowledgement_after_repeated_revocation() {
        let (id, receiver) = register_message_receipt();
        let claim = claim_message_receipt(&id).unwrap();
        let first = cancel_message_receipt_with_ack(&id).unwrap();
        let second = cancel_message_receipt_with_ack(&id).unwrap();
        assert!(!first.is_finished());
        assert!(!second.is_finished());
        assert!(claim_message_receipt(&id).is_none());
        assert!(receiver.await.is_err());
        claim.cancelled().await;
        drop(claim);
        first.wait().await;
        second.wait().await;
        assert!(!cancel_message_receipt(&id));
    }

    #[tokio::test]
    async fn completed_active_receipt_keeps_acknowledgement_until_claim_drops() {
        let (id, receiver) = register_message_receipt();
        let claim = claim_message_receipt(&id).unwrap();
        assert!(complete_message_receipt(&id, Ok(())));
        assert_eq!(receiver.await.unwrap(), Ok(()));
        assert!(!complete_message_receipt(&id, Ok(())));
        let ack = cancel_message_receipt_with_ack(&id).unwrap();
        assert!(!ack.is_finished());
        drop(claim);
        ack.wait().await;
        assert!(!cancel_message_receipt(&id));
    }

    #[tokio::test]
    async fn r5_live_receipt_survives_another_registration_after_600_seconds() {
        let (receipt_id, receiver) = register_message_receipt();
        receipt_registry()
            .lock()
            .unwrap()
            .get_mut(&receipt_id)
            .unwrap()
            .1 = Instant::now() - Duration::from_secs(661);
        let (other_id, _other_receiver) = register_message_receipt();
        let completed = complete_message_receipt(&receipt_id, Ok(()));
        cancel_message_receipt(&other_id);
        assert!(
            completed,
            "a live receipt with a 900-second caller deadline was expired at 600 seconds"
        );
        assert_eq!(receiver.await.unwrap(), Ok(()));
    }

    #[test]
    fn r5_abandoned_receipt_is_reclaimed_on_registration() {
        let (receipt_id, receiver) = register_message_receipt();
        drop(receiver);
        let (other_id, _other_receiver) = register_message_receipt();
        let retained = cancel_message_receipt(&receipt_id);
        cancel_message_receipt(&other_id);
        assert!(
            !retained,
            "abandoned receivers should be reclaimed without waiting 600 seconds"
        );
    }

    #[tokio::test]
    async fn registered_receipt_receives_completion_once() {
        let (receipt_id, receiver) = register_message_receipt();

        assert!(complete_message_receipt(&receipt_id, Ok(())));
        assert!(!complete_message_receipt(&receipt_id, Ok(())));
        assert_eq!(receiver.await.expect("receipt sender"), Ok(()));
    }

    #[tokio::test]
    async fn cancelled_receipt_drops_receiver() {
        let (receipt_id, receiver) = register_message_receipt();

        assert!(cancel_message_receipt(&receipt_id));
        assert!(receiver.await.is_err());
    }
}
