//! Logica di Sintonia senza interfaccia: sorgenti (M3U, Xtream Codes), guida XMLTV, database.
//!
//! In M0 contiene solo [`redact`], che serve già al player per non scrivere credenziali nei log.
//! Il resto arriva con le milestone M1-M3.

pub mod crypto;
pub mod db;
pub mod m3u;
pub mod redact;
pub mod secrets;
pub mod service;
