pub mod cloud;
pub mod config;
pub mod error;
pub mod model;
mod page_renderer;
pub mod render {
    //! Backwards-compatible page-renderer API.

    pub use crate::page_renderer::PageRenderer;
}
pub mod server;

pub use server::RemarkableServer;
