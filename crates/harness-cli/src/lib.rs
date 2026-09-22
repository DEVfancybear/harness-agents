#![forbid(unsafe_code)]

//! The parts of `ha` an integration test has to drive directly.
//!
//! The product is a binary, and most of it is exercised by running that binary.
//! Two surfaces cannot be: the loopback web server owns a listening socket and
//! a spawned task, so a test has to start it in-process to observe it, and the
//! child-process fixtures have to be spawned by path. Exposing exactly those
//! keeps the binary the product while making the behaviour testable without a
//! second entry point.

pub mod daemon;
pub mod web;
