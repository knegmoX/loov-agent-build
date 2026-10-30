//! 飞书长连接的帧：protobuf（pbbp2），只有两种消息，手写编解码，不引入 protobuf 依赖。
//!
//! ```text
//! message Header { string key = 1; string value = 2; }
//! message Frame  { uint64 SeqID = 1; uint64 LogID = 2; int32 service = 3; int32 method = 4;
//!                  repeated Header headers = 5; string payload_encoding = 6; string payload_type = 7;
//!                  bytes payload = 8; string LogIDNew = 9; }
//! ```
//! method 0 是控制帧（ping / pong），1 是数据帧（事件）。

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Frame {
    pub seq_id: u64,
    pub log_id: u64,
    pub service: i32,
    pub method: i32,
    pub headers: Vec<(String, String)>,
    pub payload_encoding: String,
    pub payload_type: String,
    pub payload: Vec<u8>,
    pub log_id_new: String,
}

pub const METHOD_CONTROL: i32 = 0;
pub const METHOD_DATA: i32 = 1;

impl Frame {
    pub fn header(&self, key: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    /// ping 控制帧。
    pub fn ping(service: i32) -> Frame {
        Frame { service, method: METHOD_CONTROL, headers: vec![("type".into(), "ping".into())], ..Default::default() }
    }

    /// 数据帧的应答：原帧照抄，payload 换成 `{"code":200}`（3 秒内不回会被重推）。
    pub fn ack(&self) -> Frame {
        Frame { payload: br#"{"code":200}"#.to_vec(), ..self.clone() }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        put_varint_field(&mut out, 1, self.seq_id);
        put_varint_field(&mut out, 2, self.log_id);
        put_varint_field(&mut out, 3, self.service as i64 as u64);
        put_varint_field(&mut out, 4, self.method as i64 as u64);
        for (k, v) in &self.headers {
            let mut h = Vec::new();
            put_bytes_field(&mut h, 1, k.as_bytes());
            put_bytes_field(&mut h, 2, v.as_bytes());
            put_bytes_field(&mut out, 5, &h);
        }
        if !self.payload_encoding.is_empty() {
            put_bytes_field(&mut out, 6, self.payload_encoding.as_bytes());
        }
        if !self.payload_type.is_empty() {
            put_bytes_field(&mut out, 7, self.payload_type.as_bytes());
        }
        if !self.payload.is_empty() {
            put_bytes_field(&mut out, 8, &self.payload);
        }
        if !self.log_id_new.is_empty() {
            put_bytes_field(&mut out, 9, self.log_id_new.as_bytes());
        }
        out
    }

    pub fn decode(buf: &[u8]) -> Result<Frame, String> {
        let mut f = Frame::default();
        let mut pos = 0;
        while pos < buf.len() {
            let key = read_varint(buf, &mut pos)?;
            let (field, wire) = (key >> 3, key & 7);
            match wire {
                0 => {
                    let v = read_varint(buf, &mut pos)?;
                    match field {
                        1 => f.seq_id = v,
                        2 => f.log_id = v,
                        3 => f.service = v as i32,
                        4 => f.method = v as i32,
                        _ => {}
                    }
                }
                2 => {
                    let bytes = read_bytes(buf, &mut pos)?;
                    match field {
                        5 => f.headers.push(decode_header(bytes)?),
                        6 => f.payload_encoding = String::from_utf8_lossy(bytes).into_owned(),
                        7 => f.payload_type = String::from_utf8_lossy(bytes).into_owned(),
                        8 => f.payload = bytes.to_vec(),
                        9 => f.log_id_new = String::from_utf8_lossy(bytes).into_owned(),
                        _ => {}
                    }
                }
                other => return Err(format!("不支持的 protobuf wire type {other}")),
            }
        }
        Ok(f)
    }
}

fn decode_header(buf: &[u8]) -> Result<(String, String), String> {
    let (mut k, mut v) = (String::new(), String::new());
    let mut pos = 0;
    while pos < buf.len() {
        let key = read_varint(buf, &mut pos)?;
        if key & 7 != 2 {
            return Err("header 字段类型错误".into());
        }
        let s = String::from_utf8_lossy(read_bytes(buf, &mut pos)?).into_owned();
        match key >> 3 {
            1 => k = s,
            2 => v = s,
            _ => {}
        }
    }
    Ok((k, v))
}

fn read_varint(buf: &[u8], pos: &mut usize) -> Result<u64, String> {
    let mut result = 0u64;
    let mut shift = 0;
    loop {
        let b = *buf.get(*pos).ok_or("帧被截断")?;
        *pos += 1;
        result |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Ok(result);
        }
        shift += 7;
        if shift > 63 {
            return Err("varint 过长".into());
        }
    }
}

fn read_bytes<'a>(buf: &'a [u8], pos: &mut usize) -> Result<&'a [u8], String> {
    let len = read_varint(buf, pos)? as usize;
    let end = pos.checked_add(len).filter(|e| *e <= buf.len()).ok_or("帧被截断")?;
    let s = &buf[*pos..end];
    *pos = end;
    Ok(s)
}

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn put_varint_field(out: &mut Vec<u8>, field: u64, v: u64) {
    put_varint(out, field << 3);
    put_varint(out, v);
}

fn put_bytes_field(out: &mut Vec<u8>, field: u64, bytes: &[u8]) {
    put_varint(out, (field << 3) | 2);
    put_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_and_acks() {
        let f = Frame {
            seq_id: 300,
            log_id: 7,
            service: 12345,
            method: METHOD_DATA,
            headers: vec![("type".into(), "event".into()), ("message_id".into(), "abc".into())],
            payload: br#"{"x":1}"#.to_vec(),
            log_id_new: "log-1".into(),
            ..Default::default()
        };
        let back = Frame::decode(&f.encode()).unwrap();
        assert_eq!(back, f);
        assert_eq!(back.header("type"), Some("event"));
        let ack = back.ack();
        assert_eq!(ack.payload, br#"{"code":200}"#);
        assert_eq!(ack.headers, f.headers);
        let ping = Frame::decode(&Frame::ping(9).encode()).unwrap();
        assert_eq!((ping.method, ping.service, ping.header("type")), (METHOD_CONTROL, 9, Some("ping")));
    }

    #[test]
    fn rejects_truncated_frames() {
        let mut bytes = Frame { payload: vec![1; 20], ..Default::default() }.encode();
        bytes.truncate(bytes.len() - 5);
        assert!(Frame::decode(&bytes).is_err());
    }
}
