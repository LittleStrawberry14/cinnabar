//! Native application ownership for a browser gateway's ephemeral account and play streams.
#![cfg(not(target_arch = "wasm32"))]

mod account;

mod backend;
mod bearer;
mod budget;
mod control;
mod play;
mod prefix;
mod registry;
mod session;
mod socket;
mod upstream;

#[cfg(test)]
mod tests;
