//! 長度前綴封包的讀寫。

use crate::{KIND_INPUT, MAX_FRAME};
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// 讀一個封包；對方正常關閉連線時回傳 `Ok(None)`。超過 16 MiB 回傳錯誤（呼叫端應關閉連線）。
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Option<(u8, Vec<u8>)>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len) as usize;
    if len == 0 || len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("封包長度不合法：{len}"),
        ));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    let kind = buf[0];
    buf.remove(0);
    Ok(Some((kind, buf)))
}

/// 編碼一個封包。
pub fn encode_frame(kind: u8, payload: &[u8]) -> io::Result<Vec<u8>> {
    let len = payload.len() + 1;
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "封包超過 16 MiB",
        ));
    }
    let mut out = Vec::with_capacity(len + 4);
    out.extend_from_slice(&(len as u32).to_be_bytes());
    out.push(kind);
    out.extend_from_slice(payload);
    Ok(out)
}

pub async fn write_frame<W: AsyncWrite + Unpin>(
    w: &mut W,
    kind: u8,
    payload: &[u8],
) -> io::Result<()> {
    let buf = encode_frame(kind, payload)?;
    w.write_all(&buf).await?;
    w.flush().await
}

/// kind 0x03 的 payload：`id_len: u8 | id | bytes`。
pub fn encode_input(id: &str, bytes: &[u8]) -> Vec<u8> {
    let id = id.as_bytes();
    let mut out = Vec::with_capacity(1 + id.len() + bytes.len());
    out.push(id.len().min(255) as u8);
    out.extend_from_slice(&id[..id.len().min(255)]);
    out.extend_from_slice(bytes);
    out
}

pub fn decode_input(payload: &[u8]) -> Option<(String, &[u8])> {
    let n = *payload.first()? as usize;
    if payload.len() < 1 + n {
        return None;
    }
    let id = std::str::from_utf8(&payload[1..1 + n]).ok()?.to_string();
    Some((id, &payload[1 + n..]))
}

pub fn input_frame(id: &str, bytes: &[u8]) -> io::Result<Vec<u8>> {
    encode_frame(KIND_INPUT, &encode_input(id, bytes))
}

/// 從累積的緩衝區中取出完整的封包。
fn take_frames(buf: &mut Vec<u8>, out: &mut Vec<(u8, Vec<u8>)>) -> io::Result<()> {
    let mut start = 0;
    while buf.len() - start >= 4 {
        let len = u32::from_be_bytes(buf[start..start + 4].try_into().unwrap()) as usize;
        if len == 0 || len > MAX_FRAME {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("封包長度不合法：{len}"),
            ));
        }
        if buf.len() - start - 4 < len {
            break;
        }
        let body = &buf[start + 4..start + 4 + len];
        out.push((body[0], body[1..].to_vec()));
        start += 4 + len;
    }
    buf.drain(..start);
    Ok(())
}

/// 在單一 task 中驅動一條連線：同時讀取封包並寫出 `out` 佇列中的資料。
///
/// 讀與寫在同一個 `select!` 中輪詢（`read` / `write` 都可以安全取消），
/// 不會有兩個 task 同時輪詢同一個 handle——Windows named pipe 在那種情況下會遺失喚醒。
/// `on_frame` 回傳 false 時，送出佇列中剩下的資料後結束。
pub async fn drive<S, F>(
    stream: &S,
    mut out: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    mut on_frame: F,
) where
    for<'a> &'a S: AsyncRead + AsyncWrite + Unpin,
    F: FnMut(u8, Vec<u8>) -> bool,
{
    let mut rd = stream;
    let mut wr = stream;
    let mut inbuf: Vec<u8> = Vec::with_capacity(64 * 1024);
    let mut chunk = vec![0u8; 64 * 1024];
    let mut pending: Vec<u8> = Vec::new();
    let mut off = 0usize;
    let mut out_open = true;
    let mut frames = Vec::new();
    let trace = std::env::var_os("AGT_TRACE_IPC").is_some();
    let pid = std::process::id();
    'outer: loop {
        if pending.len() == off && out_open {
            pending.clear();
            off = 0;
            // 合併已排隊的資料，減少系統呼叫
            while let Ok(b) = out.try_recv() {
                pending.extend_from_slice(&b);
                if pending.len() > 1 << 20 {
                    break;
                }
            }
        }
        let has_pending = pending.len() > off;
        tokio::select! {
            r = rd.read(&mut chunk) => match r {
                Ok(0) | Err(_) => {
                    if trace { eprintln!("[ipc {pid}] read EOF/err: {r:?}"); }
                    break
                }
                Ok(n) => {
                    if trace { eprintln!("[ipc {pid}] read {n} bytes"); }
                    inbuf.extend_from_slice(&chunk[..n]);
                    if take_frames(&mut inbuf, &mut frames).is_err() {
                        break;
                    }
                    for (k, p) in frames.drain(..) {
                        if !on_frame(k, p) {
                            break 'outer;
                        }
                    }
                }
            },
            w = wr.write(&pending[off..]), if has_pending => match w {
                Ok(0) | Err(_) => {
                    if trace { eprintln!("[ipc {pid}] write err {w:?}"); }
                    return
                }
                Ok(n) => {
                    if trace { eprintln!("[ipc {pid}] wrote {n}/{} bytes", pending.len() - off); }
                    off += n
                }
            },
            m = out.recv(), if !has_pending && out_open => match m {
                Some(b) => {
                    pending.clear();
                    off = 0;
                    pending.extend_from_slice(&b);
                }
                None => out_open = false,
            },
        }
    }
    // 送出剩下的資料（例如版本不符的錯誤回應）
    while let Ok(b) = out.try_recv() {
        pending.extend_from_slice(&b);
    }
    if pending.len() > off {
        let _ = wr.write_all(&pending[off..]).await;
    }
    let _ = wr.flush().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_from_partial_buffer() {
        let mut buf = encode_frame(1, b"ab").unwrap();
        let second = encode_frame(2, b"cde").unwrap();
        buf.extend_from_slice(&second[..4]);
        let mut out = vec![];
        take_frames(&mut buf, &mut out).unwrap();
        assert_eq!(out, vec![(1, b"ab".to_vec())]);
        buf.extend_from_slice(&second[4..]);
        take_frames(&mut buf, &mut out).unwrap();
        assert_eq!(out[1], (2, b"cde".to_vec()));
        assert!(buf.is_empty());
    }

    #[tokio::test]
    async fn roundtrip() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        write_frame(&mut a, 1, b"hello").await.unwrap();
        drop(a);
        let (k, p) = read_frame(&mut b).await.unwrap().unwrap();
        assert_eq!((k, p.as_slice()), (1, &b"hello"[..]));
        assert!(read_frame(&mut b).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn rejects_oversize() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&((MAX_FRAME as u32) + 1).to_be_bytes())
            .await
            .unwrap();
        assert!(read_frame(&mut b).await.is_err());
    }

    #[test]
    fn input_payload() {
        let p = encode_input("abc", "中".as_bytes());
        let (id, b) = decode_input(&p).unwrap();
        assert_eq!(id, "abc");
        assert_eq!(b, "中".as_bytes());
    }
}
