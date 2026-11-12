//! The byte source the paged reader reads through. The types live in
//! `datatab_byte_source` so readers of other file formats share them.

#[cfg(any(unix, windows))]
pub use datatab_byte_source::FileSource;
pub use datatab_byte_source::{ByteSource, MemorySource, SourceError};
