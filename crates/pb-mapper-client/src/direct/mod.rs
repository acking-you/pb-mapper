//! Client-owned direct sessions. Control sockets never own an established data task.
mod candidates;
mod conn;
mod consumer;
mod control;
mod endpoint;
mod identity;
mod lan;
mod link;
#[cfg(feature = "port-mapping")]
mod mapping;
mod provider;
mod reports;
mod socket;
mod tcp;

pub(crate) use consumer::Consumer;
pub(crate) use provider::Provider;

#[cfg(test)]
mod tests;
