//! Concrete open-source Gowin toolchain backends.

mod device;
mod error;
mod filesystem;
mod observer;
mod pack;
mod pnr;
mod process;
mod programmer;
mod synthesis;
mod toolchain;

pub use device::{GowinConfigError, GowinDevice};
pub use error::ToolError;
pub use observer::{ProcessObserver, ProcessStream};
pub use pack::GowinPackBackend;
pub use pnr::{GowinPnrBackend, RoutedNetlist};
pub use programmer::{OpenFpgaloaderBackend, openfpgaloader_arguments};
pub use synthesis::GowinSynthesisBackend;
pub use toolchain::Toolchain;
