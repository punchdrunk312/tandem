pub mod channel;
pub mod discovery;
pub mod error;
pub mod files;
pub mod image;
pub mod keymap;
pub mod license;
#[cfg(target_os = "macos")]
pub mod mac;
pub mod mock;
pub mod node;
pub mod platform;
pub mod proto;
pub mod span;
#[cfg(windows)]
pub mod win;
#[cfg(windows)]
mod winspan;

pub use error::{Error, Result};

/// The real input layer on Windows and macOS; an in-memory stand-in elsewhere (so the
/// rest of the app can be developed and tested on any OS).
pub fn default_platform() -> std::sync::Arc<dyn platform::Platform> {
    #[cfg(windows)]
    {
        std::sync::Arc::new(win::WinPlatform::new())
    }
    #[cfg(target_os = "macos")]
    {
        std::sync::Arc::new(mac::MacPlatform::new())
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        mock::MockPlatform::new(1920, 1080)
    }
}
