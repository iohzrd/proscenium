mod client;
mod orchestration;
mod processing;
mod server;

pub use orchestration::{sync_from_own_device, sync_one_peer};
pub use server::handle_sync;

#[cfg(test)]
mod tests;
