mod admin;
mod server;

pub use admin::{AdminHandler, DashboardDeps};
pub use server::{AdminServer, manifest_path};
