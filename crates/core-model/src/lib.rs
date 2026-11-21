//! Pure data model for Tether: vault entities, hybrid logical clocks and
//! connection-settings inheritance. No I/O, no UI, no crypto.

mod entities;
mod hlc;
mod settings;

pub use entities::*;
pub use hlc::*;
pub use settings::*;
