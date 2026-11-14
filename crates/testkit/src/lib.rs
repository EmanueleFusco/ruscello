//! ruscello-testkit
//!
//! Test utilities: local HTTP test server with failure scenarios, fake hoster pages, fixtures. Dev-dependency only.
//!
//! Each server listens on an ephemeral loopback port. Register deterministic file
//! scenarios with [TestServer::file] and inspect the observed requests through the
//! returned [TestFile].

mod fixture;
mod server;

pub use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
pub use fixture::{FileSpec, Payload, Representation};
pub use server::{Reply, Request, RequestRecord, TestFile, TestServer};
