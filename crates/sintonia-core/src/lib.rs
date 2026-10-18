//! Logica di Sintonia senza interfaccia: sorgenti (M3U, Xtream Codes), archivio, servizio di I/O.
//! La guida XMLTV arriva con M3.

pub mod crypto;
pub mod db;
pub mod m3u;
pub mod redact;
pub mod secrets;
pub mod service;
pub mod xtream;
#[cfg(any(test, feature = "mock"))]
pub mod xtream_mock;
