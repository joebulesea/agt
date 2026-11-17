//! agt 的 IPC 協定：訊息型別、封包編解碼、傳輸層與共用的 client。
//!
//! 封包格式（規格 4.2）：`len: u32 (big endian) | kind: u8 | payload (len-1 bytes)`。

pub mod client;
pub mod codec;
pub mod msg;
pub mod screen;
pub mod text;
pub mod transport;
pub mod types;

pub use msg::*;
pub use screen::*;
pub use types::*;

/// 協定版本；只在不相容變更時加一（規格 4.4）。
pub const PROTO_VERSION: u32 = 1;

/// 封包類型。
pub const KIND_JSON: u8 = 0x01;
pub const KIND_SCREEN: u8 = 0x02;
pub const KIND_INPUT: u8 = 0x03;

/// 單一封包上限 16 MiB。
pub const MAX_FRAME: usize = 16 * 1024 * 1024;
/// 終端機大小上限。client 送來的大小會直接變成畫面緩衝區，不設限的話一個 `Resize`
/// （65535 × 65535）就能讓 daemon 耗盡記憶體。
pub const MAX_COLS: u16 = 1000;
pub const MAX_ROWS: u16 = 500;
