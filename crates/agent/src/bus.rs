use blockcell_core::{InboundMessage, OutboundMessage};
use tokio::sync::mpsc;

pub struct MessageBus {
    pub inbound_tx: mpsc::Sender<InboundMessage>,
    pub inbound_rx: mpsc::Receiver<InboundMessage>,
    pub outbound_tx: mpsc::Sender<OutboundMessage>,
    pub outbound_rx: mpsc::Receiver<OutboundMessage>,
}

impl MessageBus {
    pub fn new(buffer_size: usize) -> Self {
        let (inbound_tx, inbound_rx) = mpsc::channel(buffer_size);
        let (outbound_tx, outbound_rx) = mpsc::channel(buffer_size);
        Self {
            inbound_tx,
            inbound_rx,
            outbound_tx,
            outbound_rx,
        }
    }

    pub fn split(
        self,
    ) -> (
        (mpsc::Sender<InboundMessage>, mpsc::Receiver<InboundMessage>),
        (mpsc::Sender<OutboundMessage>, mpsc::Receiver<OutboundMessage>),
    ) {
        (
            (self.inbound_tx, self.inbound_rx),
            (self.outbound_tx, self.outbound_rx),
        )
    }
}
