//! Media I/O built on the ffmpeg command line: decoding, audio extraction,
//! playback clock and encoding. No native library linking required.

pub mod audio;
pub mod decoder;
pub mod export;
pub mod ffmpeg;
pub mod player;
pub mod sync;
