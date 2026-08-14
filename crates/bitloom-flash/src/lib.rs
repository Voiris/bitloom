//! Partition layout parsing, flash image assembly and FPGA programming tasks.

mod blpl;
mod existing;
mod flash_image;
mod model;
mod path;
mod program;
mod rollback;
mod sram_image;

pub use blpl::{BlplDocument, BlplError, BlplParseTask};
pub use existing::{ExistingBitstreamError, ExistingBitstreamTask};
pub use flash_image::{
    FlashImageDependencies, FlashImageError, FlashImageInputs, FlashImageTask, NamedBitstream,
};
pub use model::{ProgramImage, ProgramMode};
pub use path::validate_project_relative;
pub use program::{ProgramTask, ProgrammerBackend};
pub use sram_image::{SramImageDependencies, SramImageError, SramImageTask};

#[cfg(test)]
mod tests;
