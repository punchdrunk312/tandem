pub mod channel;
pub mod discovery;
pub mod error;
pub mod files;
pub mod image;
pub mod mock;
pub mod node;
pub mod platform;
pub mod proto;
#[cfg(windows)]
pub mod win;

pub use error::{Error, Result};

/// The real input layer on Windows; an in-memory stand-in elsewhere (so the
/// rest of the app can be developed and tested on any OS).
pub fn default_platform() -> std::sync::Arc<dyn platform::Platform> {
    #[cfg(windows)]
    {
        std::sync::Arc::new(win::WinPlatform::new())
    }
    #[cfg(not(windows))]
    {
        mock::MockPlatform::new(1920, 1080)
    }
}
