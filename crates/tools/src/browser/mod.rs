//! CDP-based browser automation module.
//!
//! Architecture inspired by agent-browser (Vercel):
//! - Daemon model: Chrome persists between tool calls via SessionManager
//! - CDP protocol: Full Chrome DevTools Protocol over WebSocket
//! - Accessibility Snapshot + Ref system: AI-friendly element targeting
//! - Session isolation: Multiple independent browser sessions

pub mod backend;
pub mod cdp;
pub mod cdp_backend;
pub mod session;
pub mod snapshot;
pub mod tool;

pub use backend::{BrowserBackend, BrowserRequest};
pub use cdp_backend::CdpBrowserBackend;
pub use tool::BrowseTool;
