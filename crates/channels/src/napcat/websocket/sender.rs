//! Global sender for NapCatQQ outbound messages.
//!
//! This module provides a mechanism to send messages via WebSocket when
//! in ws-client or ws-server mode, bypassing HTTP entirely.

use std::collections::HashMap;
use std::sync::OnceLock;
use tokio::sync::{mpsc, oneshot};

use super::super::types::{ApiRequest, ApiResponse, StreamChunkData};

/// Maximum capacity for the outbound message queue.
const QUEUE_CAPACITY: usize = 256;

/// Global outbound message sender.
/// Set when the WebSocket client/server starts.
static OUTBOUND_SENDER: OnceLock<mpsc::Sender<OutboundMessage>> = OnceLock::new();

/// Global API caller for request-response pattern.
/// Set when the WebSocket client starts.
static API_CALLER: OnceLock<mpsc::Sender<ApiCallRequest>> = OnceLock::new();

/// Global stream chunk receiver.
/// Set when the WebSocket client starts.
static STREAM_CALLER: OnceLock<mpsc::Sender<StreamCallRequest>> = OnceLock::new();

/// Internal message type for outbound queue.
#[derive(Debug, Clone)]
pub struct OutboundMessage {
    /// The API request to send.
    pub request: ApiRequest,
    /// Optional self_id for server mode (to route to specific connection).
    /// If None, the message will be sent to the first available connection.
    pub self_id: Option<String>,
}

/// API call request with response channel.
#[derive(Debug)]
pub struct ApiCallRequest {
    /// The API request to send.
    pub request: ApiRequest,
    /// Response channel (sends ApiResponse on success, or error string on failure).
    pub response_tx: oneshot::Sender<ApiResponse>,
}

/// Stream call request for streaming API responses.
#[derive(Debug)]
pub struct StreamCallRequest {
    /// The API request to send.
    pub request: ApiRequest,
    /// Response channel for each chunk.
    pub chunk_tx: mpsc::Sender<StreamChunkData>,
    /// Channel to signal completion or error.
    pub done_tx: oneshot::Sender<Result<(), String>>,
}

/// Initialize the global outbound sender.
/// Returns the receiver that the WebSocket loop should use.
pub fn init_sender() -> mpsc::Receiver<OutboundMessage> {
    let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
    // This should only be called once, but if called multiple times, we keep the first one
    let _ = OUTBOUND_SENDER.set(tx);
    rx
}

/// Initialize the global API caller.
/// Returns the receiver that the WebSocket loop should use.
pub fn init_api_caller() -> mpsc::Receiver<ApiCallRequest> {
    let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
    let _ = API_CALLER.set(tx);
    rx
}

/// Initialize the global stream caller.
/// Returns the receiver that the WebSocket loop should use.
pub fn init_stream_caller() -> mpsc::Receiver<StreamCallRequest> {
    let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
    let _ = STREAM_CALLER.set(tx);
    rx
}

/// Get the global outbound sender.
/// Returns None if WebSocket mode hasn't been initialized.
pub fn get_sender() -> Option<mpsc::Sender<OutboundMessage>> {
    OUTBOUND_SENDER.get().cloned()
}

/// Check if WebSocket sender is available.
pub fn is_ws_mode() -> bool {
    OUTBOUND_SENDER.get().is_some()
}

/// Check if WebSocket API caller is available.
pub fn is_ws_api_available() -> bool {
    API_CALLER.get().is_some()
}

/// Check if WebSocket stream caller is available.
pub fn is_ws_stream_available() -> bool {
    STREAM_CALLER.get().is_some()
}

/// Send a message via WebSocket.
/// Returns an error if WebSocket mode is not active.
pub async fn send_via_ws(request: ApiRequest) -> Result<(), String> {
    let sender = get_sender().ok_or("WebSocket not connected")?;
    let msg = OutboundMessage {
        request,
        self_id: None, // For client mode, self_id is not needed
    };
    sender
        .send(msg)
        .await
        .map_err(|e| format!("Failed to queue message: {}", e))
}

/// Send a message via WebSocket with a specific self_id (for server mode).
/// Returns an error if WebSocket mode is not active.
pub async fn send_via_ws_with_self_id(request: ApiRequest, self_id: Option<String>) -> Result<(), String> {
    let sender = get_sender().ok_or("WebSocket not connected")?;
    let msg = OutboundMessage {
        request,
        self_id,
    };
    sender
        .send(msg)
        .await
        .map_err(|e| format!("Failed to queue message: {}", e))
}

/// Call an API via WebSocket and wait for response.
/// Returns the API response or an error.
pub async fn call_api_via_ws(request: ApiRequest) -> Result<ApiResponse, String> {
    let caller = API_CALLER.get().cloned().ok_or("WebSocket API caller not available")?;

    let (response_tx, response_rx) = oneshot::channel();

    let call_request = ApiCallRequest {
        request,
        response_tx,
    };

    caller
        .send(call_request)
        .await
        .map_err(|e| format!("Failed to queue API call: {}", e))?;

    // Wait for response with timeout
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        response_rx
    )
    .await
    .map_err(|_| "API call timeout".to_string())?;

    result.map_err(|_| "API response channel closed".to_string())
}

/// Call a streaming API via WebSocket and collect all chunks.
/// Returns the combined file data or an error.
///
/// This function:
/// 1. Sends the API request
/// 2. Receives multiple chunk responses
/// 3. Decodes Base64 chunk data
/// 4. Combines all chunks in order
/// 5. Returns the complete file data
pub async fn call_stream_api_via_ws(request: ApiRequest) -> Result<Vec<u8>, String> {
    let caller = STREAM_CALLER.get().cloned().ok_or("WebSocket stream caller not available")?;

    let (chunk_tx, mut chunk_rx) = mpsc::channel::<StreamChunkData>(64);
    let (done_tx, mut done_rx) = oneshot::channel::<Result<(), String>>();

    let call_request = StreamCallRequest {
        request,
        chunk_tx,
        done_tx,
    };

    caller
        .send(call_request)
        .await
        .map_err(|e| format!("Failed to queue stream call: {}", e))?;

    // Collect chunks
    let mut chunks: HashMap<i32, Vec<u8>> = HashMap::new();
    let mut total_chunks: Option<i32> = None;
    let mut stream_id: Option<String> = None;
    let mut done_result: Option<Result<(), String>> = None;

    loop {
        // Check if we have all chunks
        if let Some(total) = total_chunks {
            if chunks.len() == total as usize {
                // We have all chunks, combine and return
                let mut combined = Vec::new();
                for i in 0..total {
                    if let Some(data) = chunks.remove(&i) {
                        combined.extend_from_slice(&data);
                    }
                }

                tracing::info!(
                    total_chunks = total,
                    total_size = combined.len(),
                    "Stream download completed"
                );

                return Ok(combined);
            }
        }

        tokio::select! {
            // Receive chunk
            chunk = chunk_rx.recv() => {
                match chunk {
                    Some(chunk_data) => {
                        // Verify stream_id consistency
                        if let Some(ref sid) = stream_id {
                            if sid != &chunk_data.stream_id {
                                return Err(format!(
                                    "Stream ID mismatch: expected {}, got {}",
                                    sid, chunk_data.stream_id
                                ));
                            }
                        } else {
                            stream_id = Some(chunk_data.stream_id.clone());
                        }

                        // Track total chunks
                        if total_chunks.is_none() {
                            total_chunks = Some(chunk_data.total_chunks);
                        }

                        // Decode chunk data
                        let decoded = chunk_data.decode_data()
                            .map_err(|e| format!("Failed to decode chunk {}: {}", chunk_data.chunk_index, e))?;

                        tracing::debug!(
                            stream_id = %chunk_data.stream_id,
                            chunk_index = chunk_data.chunk_index,
                            total_chunks = chunk_data.total_chunks,
                            chunk_size = decoded.len(),
                            "Received stream chunk"
                        );

                        chunks.insert(chunk_data.chunk_index, decoded);
                    }
                    None => {
                        // Channel closed - check if done signal was already received
                        if let Some(ref result) = done_result {
                            match result {
                                Ok(()) => {
                                    // Continue to combine chunks
                                }
                                Err(e) => {
                                    return Err(e.clone());
                                }
                            }
                        } else {
                            return Err("Stream channel closed before completion".to_string());
                        }
                    }
                }
            }

            // Done signal - use fuse to prevent reuse
            result = &mut done_rx => {
                match result {
                    Ok(Ok(())) => {
                        done_result = Some(Ok(()));
                        // Continue loop to collect remaining chunks
                    }
                    Ok(Err(e)) => {
                        return Err(e);
                    }
                    Err(_) => {
                        return Err("Stream done channel closed unexpectedly".to_string());
                    }
                }
            }

            // Timeout
            _ = tokio::time::sleep(std::time::Duration::from_secs(120)) => {
                return Err("Stream download timeout".to_string());
            }
        }
    }
}